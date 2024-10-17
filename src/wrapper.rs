use std::{
    fmt::Debug,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    progress::{IndicatifHandler, ProgressHandler},
    pyfunc::{ExpandDtype, PyModel, PyVariable, TensorShape},
    pymc::{ExpandFunc, LogpFunc, PyMcModel},
    stan::{StanLibrary, StanModel},
};

use anyhow::{bail, Context, Result};
use arrow::array::Array;
use nuts_rs::{
    ChainProgress, DiagGradNutsSettings, LowRankNutsSettings, ProgressCallback, Sampler,
    SamplerWaitResult, Trace, TransformedNutsSettings,
};
use pyo3::{
    exceptions::PyTimeoutError,
    ffi::Py_uintptr_t,
    prelude::*,
    types::{PyList, PyTuple},
};
use rand::{thread_rng, RngCore};

#[pyclass]
struct PyChainProgress(ChainProgress);

#[pymethods]
impl PyChainProgress {
    #[getter]
    fn finished_draws(&self) -> usize {
        self.0.finished_draws
    }

    #[getter]
    fn total_draws(&self) -> usize {
        self.0.total_draws
    }

    #[getter]
    fn divergences(&self) -> usize {
        self.0.divergences
    }

    #[getter]
    fn started(&self) -> bool {
        self.0.started
    }

    #[getter]
    fn tuning(&self) -> bool {
        self.0.tuning
    }

    #[getter]
    fn num_steps(&self) -> usize {
        self.0.latest_num_steps
    }

    #[getter]
    fn step_size(&self) -> f64 {
        self.0.step_size
    }
}

#[pyclass]
#[derive(Clone)]
pub struct PyNutsSettings {
    inner: Settings,
}

#[derive(Clone, Debug)]
enum Settings {
    Diag(DiagGradNutsSettings),
    LowRank(LowRankNutsSettings),
    Transforming(TransformedNutsSettings),
}

impl PyNutsSettings {
    fn new_diag(seed: Option<u64>) -> Self {
        let seed = seed.unwrap_or_else(|| {
            let mut rng = thread_rng();
            rng.next_u64()
        });
        let settings = DiagGradNutsSettings {
            seed,
            ..Default::default()
        };

        Self {
            inner: Settings::Diag(settings),
        }
    }

    fn new_low_rank(seed: Option<u64>) -> Self {
        let seed = seed.unwrap_or_else(|| {
            let mut rng = thread_rng();
            rng.next_u64()
        });
        let settings = LowRankNutsSettings {
            seed,
            ..Default::default()
        };

        Self {
            inner: Settings::LowRank(settings),
        }
    }

    fn new_tranform_adapt(seed: Option<u64>) -> Self {
        let seed = seed.unwrap_or_else(|| {
            let mut rng = thread_rng();
            rng.next_u64()
        });
        let settings = TransformedNutsSettings {
            seed,
            ..Default::default()
        };

        Self {
            inner: Settings::Transforming(settings),
        }
    }
}

// TODO switch to serde to expose all the options...
#[pymethods]
impl PyNutsSettings {
    #[staticmethod]
    #[allow(non_snake_case)]
    fn Diag(seed: Option<u64>) -> Self {
        PyNutsSettings::new_diag(seed)
    }

    #[staticmethod]
    #[allow(non_snake_case)]
    fn LowRank(seed: Option<u64>) -> Self {
        PyNutsSettings::new_low_rank(seed)
    }

    #[staticmethod]
    #[allow(non_snake_case)]
    fn Transform(seed: Option<u64>) -> Self {
        PyNutsSettings::new_tranform_adapt(seed)
    }

    #[getter]
    fn num_tune(&self) -> u64 {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.num_tune,
            Settings::LowRank(nuts_settings) => nuts_settings.num_tune,
            Settings::Transforming(nuts_settings) => nuts_settings.num_tune,
        }
    }

    #[setter(num_tune)]
    fn set_num_tune(&mut self, val: u64) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.num_tune = val,
            Settings::LowRank(nuts_settings) => nuts_settings.num_tune = val,
            Settings::Transforming(nuts_settings) => nuts_settings.num_tune = val,
        }
    }

    #[getter]
    fn num_chains(&self) -> usize {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.num_chains,
            Settings::LowRank(nuts_settings) => nuts_settings.num_chains,
            Settings::Transforming(nuts_settings) => nuts_settings.num_chains,
        }
    }

    #[setter(num_chains)]
    fn set_num_chains(&mut self, val: usize) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.num_chains = val,
            Settings::LowRank(nuts_settings) => nuts_settings.num_chains = val,
            Settings::Transforming(nuts_settings) => nuts_settings.num_chains = val,
        }
    }

    #[getter]
    fn num_draws(&self) -> u64 {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.num_draws,
            Settings::LowRank(nuts_settings) => nuts_settings.num_draws,
            Settings::Transforming(nuts_settings) => nuts_settings.num_draws,
        }
    }

    #[setter(num_draws)]
    fn set_num_draws(&mut self, val: u64) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.num_draws = val,
            Settings::LowRank(nuts_settings) => nuts_settings.num_draws = val,
            Settings::Transforming(nuts_settings) => nuts_settings.num_draws = val,
        }
    }

    #[getter]
    fn window_switch_freq(&self) -> Result<u64> {
        match &self.inner {
            Settings::Diag(nuts_settings) => {
                Ok(nuts_settings.adapt_options.mass_matrix_switch_freq)
            }
            Settings::LowRank(nuts_settings) => {
                Ok(nuts_settings.adapt_options.mass_matrix_switch_freq)
            }
            Settings::Transforming(_) => {
                bail!("Option window_switch_freq not availbale for transformation adaptation")
            }
        }
    }

    #[setter(window_switch_freq)]
    fn set_window_switch_freq(&mut self, val: u64) -> Result<()> {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => {
                nuts_settings.adapt_options.mass_matrix_switch_freq = val;
                Ok(())
            }
            Settings::LowRank(nuts_settings) => {
                nuts_settings.adapt_options.mass_matrix_switch_freq = val;
                Ok(())
            }
            Settings::Transforming(_) => {
                bail!("Option window_switch_freq not availbale for transformation adaptation")
            }
        }
    }

    #[getter]
    fn early_window_switch_freq(&self) -> Result<u64> {
        match &self.inner {
            Settings::Diag(nuts_settings) => {
                Ok(nuts_settings.adapt_options.early_mass_matrix_switch_freq)
            }
            Settings::LowRank(nuts_settings) => {
                Ok(nuts_settings.adapt_options.early_mass_matrix_switch_freq)
            }
            Settings::Transforming(_) => {
                bail!("Option early_window_switch_freq not availbale for transformation adaptation")
            }
        }
    }

    #[setter(early_window_switch_freq)]
    fn set_early_window_switch_freq(&mut self, val: u64) -> Result<()> {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => {
                nuts_settings.adapt_options.early_mass_matrix_switch_freq = val;
                Ok(())
            }
            Settings::LowRank(nuts_settings) => {
                nuts_settings.adapt_options.early_mass_matrix_switch_freq = val;
                Ok(())
            }
            Settings::Transforming(_) => {
                bail!("Option early_window_switch_freq not availbale for transformation adaptation")
            }
        }
    }

    #[getter]
    fn initial_step(&self) -> f64 {
        match &self.inner {
            Settings::Diag(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .initial_step
            }
            Settings::LowRank(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .initial_step
            }
            Settings::Transforming(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .initial_step
            }
        }
    }

    #[setter(initial_step)]
    fn set_initial_step(&mut self, val: f64) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .initial_step = val;
            }
            Settings::LowRank(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .initial_step = val;
            }
            Settings::Transforming(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .initial_step = val;
            }
        }
    }

    #[getter]
    fn maxdepth(&self) -> u64 {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.maxdepth,
            Settings::LowRank(nuts_settings) => nuts_settings.maxdepth,
            Settings::Transforming(nuts_settings) => nuts_settings.maxdepth,
        }
    }

    #[setter(maxdepth)]
    fn set_maxdepth(&mut self, val: u64) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.maxdepth = val,
            Settings::LowRank(nuts_settings) => nuts_settings.maxdepth = val,
            Settings::Transforming(nuts_settings) => nuts_settings.maxdepth = val,
        }
    }

    #[getter]
    fn store_gradient(&self) -> bool {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.store_gradient,
            Settings::LowRank(nuts_settings) => nuts_settings.store_gradient,
            Settings::Transforming(nuts_settings) => nuts_settings.store_gradient,
        }
    }

    #[setter(store_gradient)]
    fn set_store_gradient(&mut self, val: bool) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.store_gradient = val,
            Settings::LowRank(nuts_settings) => nuts_settings.store_gradient = val,
            Settings::Transforming(nuts_settings) => nuts_settings.store_gradient = val,
        }
    }

    #[getter]
    fn store_unconstrained(&self) -> bool {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.store_unconstrained,
            Settings::LowRank(nuts_settings) => nuts_settings.store_unconstrained,
            Settings::Transforming(nuts_settings) => nuts_settings.store_unconstrained,
        }
    }

    #[setter(store_unconstrained)]
    fn set_store_unconstrained(&mut self, val: bool) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.store_unconstrained = val,
            Settings::LowRank(nuts_settings) => nuts_settings.store_unconstrained = val,
            Settings::Transforming(nuts_settings) => nuts_settings.store_unconstrained = val,
        }
    }

    #[getter]
    fn store_divergences(&self) -> bool {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.store_divergences,
            Settings::LowRank(nuts_settings) => nuts_settings.store_divergences,
            Settings::Transforming(nuts_settings) => nuts_settings.store_divergences,
        }
    }

    #[setter(store_divergences)]
    fn set_store_divergences(&mut self, val: bool) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.store_divergences = val,
            Settings::LowRank(nuts_settings) => nuts_settings.store_divergences = val,
            Settings::Transforming(nuts_settings) => nuts_settings.store_divergences = val,
        }
    }

    #[getter]
    fn max_energy_error(&self) -> f64 {
        match &self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.max_energy_error,
            Settings::LowRank(nuts_settings) => nuts_settings.max_energy_error,
            Settings::Transforming(nuts_settings) => nuts_settings.max_energy_error,
        }
    }

    #[setter(max_energy_error)]
    fn set_max_energy_error(&mut self, val: f64) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => nuts_settings.max_energy_error = val,
            Settings::LowRank(nuts_settings) => nuts_settings.max_energy_error = val,
            Settings::Transforming(nuts_settings) => nuts_settings.max_energy_error = val,
        }
    }

    #[getter]
    fn set_target_accept(&self) -> f64 {
        match &self.inner {
            Settings::Diag(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .target_accept
            }
            Settings::LowRank(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .target_accept
            }
            Settings::Transforming(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .target_accept
            }
        }
    }

    #[setter(target_accept)]
    fn target_accept(&mut self, val: f64) {
        match &mut self.inner {
            Settings::Diag(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .target_accept = val
            }
            Settings::LowRank(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .target_accept = val
            }
            Settings::Transforming(nuts_settings) => {
                nuts_settings
                    .adapt_options
                    .dual_average_options
                    .target_accept = val
            }
        }
    }

    #[getter]
    fn store_mass_matrix(&self) -> Result<bool> {
        match &self.inner {
            Settings::LowRank(settings) => {
                Ok(settings.adapt_options.mass_matrix_options.store_mass_matrix)
            }
            Settings::Diag(settings) => {
                Ok(settings.adapt_options.mass_matrix_options.store_mass_matrix)
            }
            Settings::Transforming(_) => {
                bail!("Option store_mass_matrix not availbale for transformation adaptation")
            }
        }
    }

    #[setter(store_mass_matrix)]
    fn set_store_mass_matrix(&mut self, val: bool) -> Result<()> {
        match &mut self.inner {
            Settings::LowRank(settings) => {
                settings.adapt_options.mass_matrix_options.store_mass_matrix = val;
                Ok(())
            }
            Settings::Diag(settings) => {
                settings.adapt_options.mass_matrix_options.store_mass_matrix = val;
                Ok(())
            }
            Settings::Transforming(_) => {
                bail!("Option store_mass_matrix not availbale for transformation adaptation")
            }
        }
    }

    #[getter]
    fn use_grad_based_mass_matrix(&self) -> Result<bool> {
        match &self.inner {
            Settings::LowRank(_) => {
                bail!("non-grad based mass matrix not available for low-rank adaptation")
            }
            Settings::Transforming(_) => {
                bail!("non-grad based mass matrix not available for transforming adaptation")
            }
            Settings::Diag(diag) => Ok(diag
                .adapt_options
                .mass_matrix_options
                .use_grad_based_estimate),
        }
    }

    #[setter(use_grad_based_mass_matrix)]
    fn set_use_grad_based_mass_matrix(&mut self, val: bool) -> Result<()> {
        match &mut self.inner {
            Settings::LowRank(_) => {
                bail!("non-grad based mass matrix not available for low-rank adaptation");
            }
            Settings::Transforming(_) => {
                bail!("non-grad based mass matrix not available for transforming adaptation");
            }
            Settings::Diag(diag) => {
                diag.adapt_options
                    .mass_matrix_options
                    .use_grad_based_estimate = val;
            }
        }
        Ok(())
    }

    #[getter]
    fn mass_matrix_switch_freq(&self) -> Result<u64> {
        match &self.inner {
            Settings::Diag(settings) => Ok(settings.adapt_options.mass_matrix_switch_freq),
            Settings::LowRank(settings) => Ok(settings.adapt_options.mass_matrix_switch_freq),
            Settings::Transforming(_) => {
                bail!("mass_matrix_switch_freq not available for transforming adaptation");
            }
        }
    }

    #[setter(mass_matrix_switch_freq)]
    fn set_mass_matrix_switch_freq(&mut self, val: u64) -> Result<()> {
        match &mut self.inner {
            Settings::Diag(settings) => settings.adapt_options.mass_matrix_switch_freq = val,
            Settings::LowRank(settings) => settings.adapt_options.mass_matrix_switch_freq = val,
            Settings::Transforming(_) => {
                bail!("mass_matrix_switch_freq not available for transforming adaptation");
            }
        }
        Ok(())
    }

    #[getter]
    fn mass_matrix_eigval_cutoff(&self) -> Result<f64> {
        match &self.inner {
            Settings::LowRank(inner) => Ok(inner.adapt_options.mass_matrix_options.eigval_cutoff),
            Settings::Diag(_) => {
                bail!("eigenvalue cutoff not available for diag mass matrix adaptation");
            }
            Settings::Transforming(_) => {
                bail!("eigenvalue cutoff not available for transfor adaptation");
            }
        }
    }

    #[setter(mass_matrix_eigval_cutoff)]
    fn set_mass_matrix_eigval_cutoff(&mut self, val: f64) -> Result<()> {
        match &mut self.inner {
            Settings::LowRank(inner) => inner.adapt_options.mass_matrix_options.eigval_cutoff = val,
            Settings::Diag(_) => {
                bail!("eigenvalue cutoff not available for diag mass matrix adaptation");
            }
            Settings::Transforming(_) => {
                bail!("eigenvalue cutoff not available for transfor adaptation");
            }
        }
        Ok(())
    }

    #[getter]
    fn mass_matrix_gamma(&self) -> Result<f64> {
        match &self.inner {
            Settings::LowRank(inner) => Ok(inner.adapt_options.mass_matrix_options.gamma),
            Settings::Diag(_) => {
                bail!("gamma not available for diag mass matrix adaptation");
            }
            Settings::Transforming(_) => {
                bail!("gamma not available for transform adaptation");
            }
        }
    }

    #[setter(mass_matrix_gamma)]
    fn set_mass_matrix_gamma(&mut self, val: f64) -> Result<()> {
        match &mut self.inner {
            Settings::LowRank(inner) => {
                inner.adapt_options.mass_matrix_options.gamma = val;
            }
            Settings::Diag(_) => {
                bail!("gamma not available for diag mass matrix adaptation");
            }
            Settings::Transforming(_) => {
                bail!("gamma not available for transform adaptation");
            }
        }
        Ok(())
    }
}

pub(crate) enum SamplerState {
    Running(Sampler),
    Finished(Option<Trace>),
    Empty,
}

#[derive(Clone)]
#[pyclass]
pub enum ProgressType {
    Callback {
        rate: Duration,
        n_cores: usize,
        template: String,
        callback: Py<PyAny>,
    },
    Indicatif {
        rate: Duration,
    },
    None {},
}

impl ProgressType {
    fn into_callback(self) -> Result<Option<ProgressCallback>> {
        match self {
            ProgressType::Callback {
                callback,
                rate,
                n_cores,
                template,
            } => {
                let handler = ProgressHandler::new(callback, rate, template, n_cores);
                let callback = handler.into_callback()?;

                Ok(Some(callback))
            }
            ProgressType::Indicatif { rate } => {
                let handler = IndicatifHandler::new(rate);
                Ok(Some(handler.into_callback()?))
            }
            ProgressType::None {} => Ok(None),
        }
    }
}

#[pymethods]
impl ProgressType {
    #[staticmethod]
    fn indicatif(rate: u64) -> Self {
        let rate = Duration::from_millis(rate);
        ProgressType::Indicatif { rate }
    }

    #[staticmethod]
    fn none() -> Self {
        ProgressType::None {}
    }

    #[staticmethod]
    fn template_callback(rate: u64, template: String, n_cores: usize, callback: Py<PyAny>) -> Self {
        let rate = Duration::from_millis(rate);
        ProgressType::Callback {
            callback,
            template,
            n_cores,
            rate,
        }
    }
}

#[pyclass]
struct PySampler(SamplerState);

#[pymethods]
impl PySampler {
    #[staticmethod]
    fn from_pymc(
        settings: PyNutsSettings,
        cores: usize,
        model: PyMcModel,
        progress_type: ProgressType,
    ) -> PyResult<PySampler> {
        let callback = progress_type.into_callback()?;
        match settings.inner {
            Settings::LowRank(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
            Settings::Diag(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
            Settings::Transforming(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
        }
    }

    #[staticmethod]
    fn from_stan(
        settings: PyNutsSettings,
        cores: usize,
        model: StanModel,
        progress_type: ProgressType,
    ) -> PyResult<PySampler> {
        let callback = progress_type.into_callback()?;
        match settings.inner {
            Settings::LowRank(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
            Settings::Diag(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
            Settings::Transforming(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
        }
    }

    #[staticmethod]
    fn from_pyfunc(
        settings: PyNutsSettings,
        cores: usize,
        model: PyModel,
        progress_type: ProgressType,
    ) -> PyResult<PySampler> {
        let callback = progress_type.into_callback()?;
        match settings.inner {
            Settings::LowRank(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
            Settings::Diag(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
            Settings::Transforming(settings) => {
                let sampler = Sampler::new(model, settings, cores, callback)?;
                Ok(PySampler(SamplerState::Running(sampler)))
            }
        }
    }

    fn is_finished(&mut self, py: Python<'_>) -> PyResult<bool> {
        py.allow_threads(|| {
            let state = std::mem::replace(&mut self.0, SamplerState::Empty);

            let SamplerState::Running(sampler) = state else {
                let _ = std::mem::replace(&mut self.0, state);
                return Ok(true);
            };

            match sampler.wait_timeout(Duration::from_millis(1)) {
                SamplerWaitResult::Trace(trace) => {
                    let _ = std::mem::replace(&mut self.0, SamplerState::Finished(Some(trace)));
                    Ok(true)
                }
                SamplerWaitResult::Timeout(sampler) => {
                    let _ = std::mem::replace(&mut self.0, SamplerState::Running(sampler));
                    Ok(false)
                }
                SamplerWaitResult::Err(err, trace) => {
                    let _ = std::mem::replace(&mut self.0, SamplerState::Finished(trace));
                    Err(err.into())
                }
            }
        })
    }

    fn pause(&mut self, py: Python<'_>) -> PyResult<()> {
        py.allow_threads(|| {
            if let SamplerState::Running(ref mut control) = self.0 {
                control.pause()?
            }
            Ok(())
        })
    }

    fn resume(&mut self, py: Python<'_>) -> PyResult<()> {
        py.allow_threads(|| {
            if let SamplerState::Running(ref mut control) = self.0 {
                control.resume()?
            }
            Ok(())
        })
    }

    fn wait(&mut self, py: Python<'_>, timeout_seconds: Option<f64>) -> PyResult<()> {
        py.allow_threads(|| {
            let timeout = match timeout_seconds {
                Some(val) => Some(Duration::try_from_secs_f64(val).context("Invalid timeout")?),
                None => None,
            };

            let state = std::mem::replace(&mut self.0, SamplerState::Empty);

            let SamplerState::Running(mut control) = state else {
                let _ = std::mem::replace(&mut self.0, state);
                return Ok(());
            };

            let start_time = Instant::now();
            let step = Duration::from_millis(100);

            let (final_state, retval) = loop {
                let time_so_far = Instant::now().saturating_duration_since(start_time);
                let next_timeout = match timeout {
                    Some(timeout) => {
                        let Some(remaining) = timeout.checked_sub(time_so_far) else {
                            break (
                                SamplerState::Running(control),
                                Err(PyTimeoutError::new_err(
                                    "Timeout while waiting for sampler to finish",
                                )),
                            );
                        };
                        remaining.min(step)
                    }
                    None => step,
                };

                match control.wait_timeout(next_timeout) {
                    SamplerWaitResult::Trace(trace) => {
                        break (SamplerState::Finished(Some(trace)), Ok(()))
                    }
                    SamplerWaitResult::Timeout(new_control) => {
                        control = new_control;
                    }
                    SamplerWaitResult::Err(err, trace) => {
                        break (SamplerState::Finished(trace), Err(err.into()))
                    }
                }

                if let Err(err) = Python::with_gil(|py| py.check_signals()) {
                    break (SamplerState::Running(control), Err(err));
                }
            };

            let _ = std::mem::replace(&mut self.0, final_state);
            retval
        })
    }

    fn abort(&mut self, py: Python<'_>) -> PyResult<()> {
        py.allow_threads(|| {
            let state = std::mem::replace(&mut self.0, SamplerState::Empty);

            let SamplerState::Running(control) = state else {
                let _ = std::mem::replace(&mut self.0, state);
                return Ok(());
            };

            let (result, trace) = control.abort();
            let _ = std::mem::replace(&mut self.0, SamplerState::Finished(trace));
            result?;
            Ok(())
        })
    }

    fn extract_results<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let state = std::mem::replace(&mut self.0, SamplerState::Empty);

        let SamplerState::Finished(trace) = state else {
            let _ = std::mem::replace(&mut self.0, state);
            return Err(anyhow::anyhow!("Sampler is not finished"))?;
        };

        let Some(trace) = trace else {
            return Err(anyhow::anyhow!(
                "Sampler failed and did not produce a trace"
            ))?;
        };

        trace_to_list(trace, py)
    }

    fn is_empty(&self) -> bool {
        match self.0 {
            SamplerState::Running(_) => false,
            SamplerState::Finished(_) => false,
            SamplerState::Empty => true,
        }
    }

    fn inspect<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let trace = py.allow_threads(|| {
            let SamplerState::Running(ref mut sampler) = self.0 else {
                return Err(anyhow::anyhow!("Sampler is not running"))?;
            };

            sampler.inspect_trace()
        })?;
        trace_to_list(trace, py)
    }
}

fn trace_to_list(trace: Trace, py: Python<'_>) -> PyResult<Bound<'_, PyList>> {
    let list = PyList::new_bound(
        py,
        trace
            .chains
            .into_iter()
            .map(|chain| {
                Ok(PyTuple::new_bound(
                    py,
                    [
                        export_array(py, chain.draws)?,
                        export_array(py, chain.stats)?,
                    ]
                    .into_iter(),
                ))
            })
            .collect::<Result<Vec<_>>>()?,
    );
    Ok(list)
}

fn export_array(py: Python<'_>, data: Arc<dyn Array>) -> PyResult<PyObject> {
    let pa = py.import_bound("pyarrow")?;
    let array = pa.getattr("Array")?;

    let data = data.into_data();

    let (data, schema) = arrow::ffi::to_ffi(&data).context("Could not convert to arrow ffi")?;

    let data = array
        .call_method1(
            "_import_from_c",
            (
                (&data as *const _ as Py_uintptr_t).into_py(py),
                (&schema as *const _ as Py_uintptr_t).into_py(py),
            ),
        )
        .context("Could not import arrow trace in python")?;
    Ok(data.into_py(py))
}

/// A Python module implemented in Rust.
#[pymodule]
pub fn _lib(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySampler>()?;
    m.add_class::<PyMcModel>()?;
    m.add_class::<LogpFunc>()?;
    m.add_class::<ExpandFunc>()?;
    m.add_class::<StanLibrary>()?;
    m.add_class::<StanModel>()?;
    m.add_class::<PyNutsSettings>()?;
    m.add_class::<PyChainProgress>()?;
    m.add_class::<ProgressType>()?;
    m.add_class::<TensorShape>()?;
    m.add_class::<PyModel>()?;
    m.add_class::<PyVariable>()?;
    m.add_class::<ExpandDtype>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
