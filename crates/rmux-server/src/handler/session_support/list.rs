use super::SessionSortOrder;

#[derive(Debug, Clone)]
pub(super) struct ListSessionSnapshot {
    pub(super) name: rmux_proto::SessionName,
    pub(super) id: u32,
    pub(super) created_at: i64,
    pub(super) activity_at: i64,
}

pub(super) fn sort_list_sessions(
    sessions: &mut [ListSessionSnapshot],
    sort_order: SessionSortOrder,
    reversed: bool,
) {
    sessions.sort_by(|left, right| {
        let ordering = match sort_order {
            SessionSortOrder::Index => left.id.cmp(&right.id),
            SessionSortOrder::Creation => left.created_at.cmp(&right.created_at),
            SessionSortOrder::Activity => right.activity_at.cmp(&left.activity_at),
            SessionSortOrder::Name
            | SessionSortOrder::Modifier
            | SessionSortOrder::Order
            | SessionSortOrder::Size => left.name.as_str().cmp(right.name.as_str()),
        };
        let ordering = if reversed {
            ordering.reverse()
        } else {
            ordering
        };
        if ordering.is_eq() {
            left.name.as_str().cmp(right.name.as_str())
        } else {
            ordering
        }
    });
}
