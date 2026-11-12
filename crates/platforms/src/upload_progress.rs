use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadProgress {
    ValidatingFile,
    Uploading { percent: Option<u8> },
    Processing,
    EditorMissing,
    FillingForm,
    SettingCover,
}

/// Shares the latest observed upload state with the desktop without persisting it.
#[derive(Clone, Default)]
pub struct UploadProgressRegistry {
    active: Arc<Mutex<HashMap<String, UploadProgress>>>,
}

impl UploadProgressRegistry {
    pub fn snapshot(&self) -> HashMap<String, UploadProgress> {
        self.active.lock().unwrap().clone()
    }

    pub(crate) fn begin(&self, job_id: &str) -> UploadProgressGuard {
        UploadProgressGuard {
            registry: self.clone(),
            job_id: job_id.to_owned(),
        }
    }
}

/// Removes the preview on normal completion and when the upload future is cancelled.
pub(crate) struct UploadProgressGuard {
    registry: UploadProgressRegistry,
    job_id: String,
}

impl UploadProgressGuard {
    pub(crate) fn set(&self, progress: Option<UploadProgress>) {
        let mut active = self.registry.active.lock().unwrap();
        if let Some(progress) = progress {
            active.insert(self.job_id.clone(), progress);
        } else {
            active.remove(&self.job_id);
        }
    }
}

impl Drop for UploadProgressGuard {
    fn drop(&mut self) {
        self.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_progress_removes_only_the_corresponding_job() {
        let registry = UploadProgressRegistry::default();
        let finished = registry.begin("finished");
        let ongoing = registry.begin("ongoing");
        finished.set(Some(UploadProgress::FillingForm));
        ongoing.set(Some(UploadProgress::Processing));
        drop(finished);
        assert_eq!(
            registry.snapshot(),
            HashMap::from([("ongoing".into(), UploadProgress::Processing)])
        );
    }
}
