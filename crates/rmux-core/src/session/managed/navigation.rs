use super::*;

impl ManagedSessionStore {
    /// Selects a neighboring occurrence in sparse local order, optionally only alerts.
    pub fn select_adjacent_window(
        &mut self,
        name: &SessionName,
        forward: bool,
        alerts_only: bool,
    ) -> Result<u32, RmuxError> {
        let session = self
            .session(name)
            .ok_or_else(|| RmuxError::SessionNotFound(name.to_string()))?;
        let indices = session
            .windows()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let start = indices
            .iter()
            .position(|index| *index == session.active_window_index())
            .expect("active occurrence exists");
        let count = indices.len();
        let index = (1..count)
            .map(|offset| {
                indices[if forward {
                    (start + offset) % count
                } else {
                    (start + count - offset) % count
                }]
            })
            .find(|index| {
                !alerts_only
                    || session
                        .winlink_alert_flags(*index)
                        .intersects(crate::WINLINK_ALERTFLAGS)
            })
            .ok_or_else(|| {
                RmuxError::Message(
                    if forward {
                        "no next window"
                    } else {
                        "no previous window"
                    }
                    .to_owned(),
                )
            })?;
        self.select_window(&WindowTarget::with_window(name.clone(), index))?;
        Ok(index)
    }
}
