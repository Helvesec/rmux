#[cfg(test)]
use std::sync::Arc;

use rmux_proto::RmuxError;

use super::{
    capture_surface_source, materialize_surface_frame, validate_surface_frame_size,
    CapturedSurfaceBoundary, PaneStreamSource, PaneSurfaceFingerprint, RequestHandler,
    SurfaceAdmissionCache,
};

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_surface_admission_pause(
        &self,
    ) -> Arc<super::super::SurfaceAdmissionPause> {
        let pause = Arc::new(super::super::SurfaceAdmissionPause::default());
        *self
            .surface_admission_pause
            .lock()
            .expect("surface admission pause") = Some(Arc::clone(&pause));
        pause
    }

    #[cfg(test)]
    async fn pause_after_surface_admission_validation(&self) {
        let pause = self
            .surface_admission_pause
            .lock()
            .expect("surface admission pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(not(test))]
    async fn pause_after_surface_admission_validation(&self) {}

    #[cfg(test)]
    pub(in crate::handler) fn surface_admission_materialization_count(&self) -> usize {
        self.surface_admission_materializations
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn current_surface_admission_cache(
        &self,
        pane_id: rmux_core::PaneId,
    ) -> Result<SurfaceAdmissionCache, RmuxError> {
        let subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let key = subscriptions
            .surface_driver_key_for_pane_id(pane_id)
            .ok_or_else(|| RmuxError::Server("pane surface driver not found".to_owned()))?;
        subscriptions
            .surface_drivers
            .get(&key)
            .map(super::SurfaceDriver::admission_cache)
            .ok_or_else(|| RmuxError::Server("pane surface driver not found".to_owned()))
    }

    pub(super) fn cache_surface_admission_if_revision(
        &self,
        pane_id: rmux_core::PaneId,
        expected_revision: u64,
        fingerprint: PaneSurfaceFingerprint,
        validation: Result<(), RmuxError>,
    ) -> bool {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry mutex must not be poisoned");
        let Some(key) = subscriptions.surface_driver_key_for_pane_id(pane_id) else {
            return false;
        };
        subscriptions
            .surface_drivers
            .get_mut(&key)
            .is_some_and(|driver| {
                driver.cache_admission_if_revision(expected_revision, fingerprint, validation)
            })
    }

    fn validate_surface_admission_capture(
        &self,
        source: &PaneStreamSource,
        captured: &CapturedSurfaceBoundary,
        cache: &SurfaceAdmissionCache,
    ) -> Result<(), RmuxError> {
        if captured.fingerprint == *cache.fingerprint() {
            return cache.validation();
        }
        let seed = captured.seed.as_ref().ok_or_else(|| {
            RmuxError::Server(
                "surface admission fingerprint changed without a projection".to_owned(),
            )
        })?;
        #[cfg(test)]
        self.surface_admission_materializations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        materialize_surface_frame(
            self,
            source.key.pane_id(),
            1,
            1,
            0,
            captured.boundary.next_output_sequence,
            seed,
        )
        .and_then(|frame| validate_surface_frame_size(&frame))
    }

    pub(super) async fn validate_current_surface_admission(
        &self,
        mut source: PaneStreamSource,
    ) -> Result<PaneStreamSource, RmuxError> {
        for _ in 0..super::MAX_SOURCE_CAPTURE_ATTEMPTS {
            let cache = self.current_surface_admission_cache(source.key.pane_id())?;
            let captured = capture_surface_source(&source, Some(cache.fingerprint()), false)?;
            if captured.boundary.generation != source.generation {
                source = self
                    .resolve_stream_source_for_pane(
                        source.key.pane_id(),
                        source.key.runtime_session_name(),
                    )
                    .await?;
                continue;
            }
            let validation = self.validate_surface_admission_capture(&source, &captured, &cache);
            if captured.fingerprint != *cache.fingerprint() {
                let _ = self.cache_surface_admission_if_revision(
                    source.key.pane_id(),
                    cache.revision(),
                    captured.fingerprint.clone(),
                    validation.clone(),
                );
            }
            self.pause_after_surface_admission_validation().await;
            if source.output.is_current_boundary(captured.boundary) {
                validation?;
                return Ok(source);
            }
            let current = capture_surface_source(&source, Some(&captured.fingerprint), false)?;
            if current.boundary.generation != source.generation {
                source = self
                    .resolve_stream_source_for_pane(
                        source.key.pane_id(),
                        source.key.runtime_session_name(),
                    )
                    .await?;
                continue;
            }
            let current_validation = if current.fingerprint == captured.fingerprint {
                validation
            } else {
                let current_cache = self.current_surface_admission_cache(source.key.pane_id())?;
                let validation =
                    self.validate_surface_admission_capture(&source, &current, &current_cache);
                if current.fingerprint != *current_cache.fingerprint() {
                    let _ = self.cache_surface_admission_if_revision(
                        source.key.pane_id(),
                        current_cache.revision(),
                        current.fingerprint.clone(),
                        validation.clone(),
                    );
                }
                validation
            };
            if source.output.current_generation() != source.generation {
                source = self
                    .resolve_stream_source_for_pane(
                        source.key.pane_id(),
                        source.key.runtime_session_name(),
                    )
                    .await?;
                continue;
            }
            // The second atomic surface capture is the admission
            // linearization point. Later output belongs after this admission;
            // it must not force another full projection or starve a busy pane.
            current_validation?;
            return Ok(source);
        }
        Err(RmuxError::Server(
            "pane generation changed repeatedly while validating stream admission".to_owned(),
        ))
    }
}
