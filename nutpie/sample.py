from collections import namedtuple

import numpy as np
import fastprogress
import arviz

from . import lib


CompiledModel = namedtuple(
    "CompiledModel",
    [
        "model",
        "n_dim",
        "logp_func_addr",
        "expand_draw",
        "make_user_data",
        "shape_info",
        "dims",
        "coords",
        "keep_alive",
    ]
)


def sample(
    compiled_model: CompiledModel,
    *,
    draws: int = 1000,
    tune: int = 1000,
    chains: int = 4,
    seed: int = 42,
    num_try_init=100,
    save_warmup: bool = True,
    store_divergences: bool = False,
    **kwargs,
):
    settings = lib.PySamplerArgs()
    settings.num_tune = tune

    for name, val in kwargs.items():
        setattr(settings, name, val)

    init_mean = np.zeros(compiled_model.n_dim)
    sampler = lib.PyParallelSampler(
        compiled_model.logp_func_addr,
        compiled_model.make_user_data,
        compiled_model.n_dim,
        init_mean,
        settings,
        n_chains=chains,
        n_draws=draws,
        seed=seed,
        n_try_init=num_try_init,
    )

    def do_sample():
        n_expanded = len(compiled_model.expand_draw(init_mean))
        draws_data = np.full((chains, draws + tune, n_expanded), np.nan)
        infos = []
        try:
            bar = fastprogress.progress_bar(
                sampler,  total=chains * (draws + tune)
            )
            num_divs = 0
            chains_tuning = chains
            for draw, info in bar:
                infos.append(info)
                draws_data[info.chain, info.draw, :] = compiled_model.expand_draw(draw)
                if info.draw == tune - 1:
                    chains_tuning -= 1
                if info.is_diverging and info.draw > tune:
                    num_divs += 1
                bar.comment = f" Chains in warmup: {chains_tuning}, Divergences: {num_divs}"
        except KeyboardInterrupt:
            pass
        return draws_data, infos
    
    try:
        draws_data, infos = do_sample()
    finally:
        try:
            sampler.finalize()
        except Exception as e:
            print(e)

    trace_dict = {}
    trace_dict_tune = {}
    for name, slice_, shape in zip(*compiled_model.shape_info):
        trace_dict_tune[name] = draws_data[:, :tune, slice_].reshape(
            (chains, tune) + tuple(shape)
        )
        trace_dict[name] = draws_data[:, tune:, slice_].reshape(
            (chains, draws) + tuple(shape)
        )

    stat_dtypes = {
        "index_in_trajectory": ((), np.int64),
        "mean_tree_accept": ((), np.float64),
        "depth": ((), np.int64),
        "maxdepth_reached": ((), bool),
        "logp": ((), np.float64),
        "energy": ((), np.float64),
        "diverging": ((), bool),
        "step_size": ((), np.float64),
        "step_size_bar": ((), np.float64),
        "mean_tree_accept": ((), np.float64),
        "n_steps": ((), np.int64),
    }

    # Sampler statistics that do not have extra dimensions
    simple_stats = list(stat_dtypes.keys())

    if settings.store_mass_matrix:
        stat_dtypes["mass_matrix_inv"] = (("unconstrained_parameter",), np.float64)
    if settings.store_gradient:
        stat_dtypes["gradient"] = (("unconstrained_parameter",), np.float64)
    if store_divergences:
        stat_dtypes["divergence_start"] = (("unconstrained_parameter",), np.float64)
        stat_dtypes["divergence_end"] = (("unconstrained_parameter",), np.float64)

    dim_to_length = {
        "unconstrained_parameter": compiled_model.n_dim,
    }

    stats = {}
    stats_tune = {}
    for name, (dims, dtype) in stat_dtypes.items():
        shapes = tuple(dim_to_length[name] for name in dims)
        if dtype == np.float64:
            value = np.nan
        else:
            value = 0
        stats[name] = np.full((chains, draws) + shapes, value, dtype=dtype)
        stats_tune[name] = np.full((chains, tune) + shapes, value, dtype=dtype)

    for info in infos:
        info_dict = info.as_dict()
        if info.draw < tune:
            out = stats_tune
            draw = info.draw
        else:
            out = stats
            draw = info.draw - tune
        for name in stat_dtypes:
            if name in info_dict:
                out[name][info.chain, draw] = info_dict[name]

    trace = arviz.from_dict(
        posterior=trace_dict,
        warmup_posterior=trace_dict_tune,
        save_warmup=save_warmup,
        coords=compiled_model.coords,
        dims={name: list(dim) for name, dim in compiled_model.dims.items()},
        sample_stats={name: stats[name] for name in simple_stats},
        warmup_sample_stats={name: stats_tune[name] for name in simple_stats},
    )

    for name in stats:
        if name in simple_stats:
            continue
        trace.sample_stats[name] = (
            ("chain", "draw") + stat_dtypes[name][0],
            stats[name]
        )
        if save_warmup:
            trace.warmup_sample_stats[name] = (
                ("chain", "draw") + stat_dtypes[name][0],
                stats_tune[name]
            )
    
    return trace