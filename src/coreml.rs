#[cfg(not(mlai_coreml))]
use std::path::Path;

#[cfg(mlai_coreml)]
mod native {
    use anyhow::Context;
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::path::Path;

    unsafe extern "C" {
        fn mlai_coreml_hardware_available() -> i32;
        fn mlai_coreml_neural_engine_operation_count(
            model_path: *const c_char,
            error_output: *mut *mut c_char,
        ) -> i64;
        fn mlai_coreml_load(
            model_path: *const c_char,
            batch_size: usize,
            sequence_length: usize,
            input_dim: usize,
            error_output: *mut *mut c_char,
        ) -> *mut c_void;
        fn mlai_coreml_predict(
            handle: *mut c_void,
            input_values: *const f32,
            output_values: *mut f32,
            error_output: *mut *mut c_char,
        ) -> i32;
        fn mlai_coreml_free(handle: *mut c_void);
        fn mlai_coreml_free_error(message: *mut c_char);
    }

    fn path_string(path: &Path) -> anyhow::Result<CString> {
        CString::new(path.to_string_lossy().as_bytes())
            .with_context(|| format!("Core ML path contains a NUL byte: {}", path.display()))
    }

    fn native_error(message: *mut c_char, fallback: &str) -> anyhow::Error {
        if message.is_null() {
            return anyhow::anyhow!(fallback.to_string());
        }
        let text = unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned();
        unsafe { mlai_coreml_free_error(message) };
        anyhow::anyhow!(text)
    }

    pub fn hardware_available() -> bool {
        unsafe { mlai_coreml_hardware_available() == 1 }
    }

    pub fn neural_engine_operation_count(path: &Path) -> anyhow::Result<usize> {
        let path = path_string(path)?;
        let mut error = std::ptr::null_mut();
        let count = unsafe { mlai_coreml_neural_engine_operation_count(path.as_ptr(), &mut error) };
        if count < 0 {
            Err(native_error(
                error,
                "Core ML compute-plan inspection failed",
            ))
        } else {
            Ok(count as usize)
        }
    }

    struct Model {
        handle: *mut c_void,
        batch_size: usize,
        sequence_length: usize,
        input_dim: usize,
    }

    impl Model {
        fn load(
            path: &Path,
            batch_size: usize,
            sequence_length: usize,
            input_dim: usize,
        ) -> anyhow::Result<Self> {
            let path = path_string(path)?;
            let mut error = std::ptr::null_mut();
            let handle = unsafe {
                mlai_coreml_load(
                    path.as_ptr(),
                    batch_size,
                    sequence_length,
                    input_dim,
                    &mut error,
                )
            };
            if handle.is_null() {
                return Err(native_error(error, "Core ML model load failed"));
            }
            Ok(Self {
                handle,
                batch_size,
                sequence_length,
                input_dim,
            })
        }

        fn predict_batch(&self, input: &[f32]) -> anyhow::Result<Vec<f64>> {
            let expected = self.batch_size * self.sequence_length * self.input_dim;
            anyhow::ensure!(
                input.len() == expected,
                "Core ML batch has {} values, expected {expected}",
                input.len()
            );
            let mut output = vec![0.0f32; self.batch_size];
            let mut error = std::ptr::null_mut();
            let status = unsafe {
                mlai_coreml_predict(self.handle, input.as_ptr(), output.as_mut_ptr(), &mut error)
            };
            if status != 0 {
                return Err(native_error(error, "Core ML prediction failed"));
            }
            Ok(output.into_iter().map(f64::from).collect())
        }
    }

    impl Drop for Model {
        fn drop(&mut self) {
            unsafe { mlai_coreml_free(self.handle) };
        }
    }

    pub fn predict_fixed_batches(
        path: &Path,
        sequences: &[Vec<Vec<f64>>],
        batch_size: usize,
        sequence_length: usize,
        input_dim: usize,
    ) -> anyhow::Result<Vec<f64>> {
        if sequences.is_empty() {
            return Ok(Vec::new());
        }
        let model = Model::load(path, batch_size, sequence_length, input_dim)?;
        let mut predictions = Vec::with_capacity(sequences.len());
        for batch in sequences.chunks(batch_size) {
            let mut input = vec![0.0f32; batch_size * sequence_length * input_dim];
            for (sample_index, sequence) in batch.iter().enumerate() {
                anyhow::ensure!(
                    sequence.len() == sequence_length,
                    "LSTM sequence has {} steps, expected {sequence_length}",
                    sequence.len()
                );
                for (step_index, step) in sequence.iter().enumerate() {
                    anyhow::ensure!(
                        step.len() == input_dim,
                        "LSTM step has {} features, expected {input_dim}",
                        step.len()
                    );
                    let offset = (sample_index * sequence_length + step_index) * input_dim;
                    for (destination, value) in
                        input[offset..offset + input_dim].iter_mut().zip(step)
                    {
                        anyhow::ensure!(
                            value.is_finite(),
                            "LSTM input contains a non-finite value"
                        );
                        *destination = *value as f32;
                    }
                }
            }
            let output = model.predict_batch(&input)?;
            predictions.extend(output.into_iter().take(batch.len()));
        }
        anyhow::ensure!(
            predictions.iter().all(|value| value.is_finite()),
            "Core ML returned a non-finite LSTM prediction"
        );
        Ok(predictions)
    }
}

#[cfg(mlai_coreml)]
pub use native::{hardware_available, neural_engine_operation_count, predict_fixed_batches};

#[cfg(not(mlai_coreml))]
pub fn hardware_available() -> bool {
    false
}

#[cfg(not(mlai_coreml))]
pub fn neural_engine_operation_count(_path: &Path) -> anyhow::Result<usize> {
    anyhow::bail!("Core ML is only available in Apple Silicon macOS builds")
}

#[cfg(not(mlai_coreml))]
pub fn predict_fixed_batches(
    _path: &Path,
    _sequences: &[Vec<Vec<f64>>],
    _batch_size: usize,
    _sequence_length: usize,
    _input_dim: usize,
) -> anyhow::Result<Vec<f64>> {
    anyhow::bail!("Core ML is only available in Apple Silicon macOS builds")
}
