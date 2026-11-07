use iso8601_timestamp::Timestamp;

use crate::MemberCompositeKey;

auto_derived!(
    /// Where a join request stands
    pub enum JoinRequestStatus {
        Pending,
        Approved,
        Rejected,
    }

    /// A request to join a server whose access is `Approval`.
    ///
    /// Keyed by server + user, so a person has at most one request per
    /// server (no index or migration needed); asking again renews it.
    pub struct ServerJoinRequest {
        /// Server and applicant
        #[serde(rename = "_id")]
        pub id: MemberCompositeKey,
        /// Current state
        pub status: JoinRequestStatus,
        /// When the request was made or last renewed
        pub created_at: Timestamp,
        /// Who approved or rejected it
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reviewed_by: Option<String>,
        /// When it was approved or rejected
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reviewed_at: Option<Timestamp>,
    }
);

/// How long someone waits before asking again after a rejection
pub const JOIN_REQUEST_COOLDOWN_SECONDS: i64 = 24 * 60 * 60;

impl ServerJoinRequest {
    /// A fresh pending request
    pub fn pending(server: &str, user: &str) -> Self {
        ServerJoinRequest {
            id: MemberCompositeKey {
                server: server.to_string(),
                user: user.to_string(),
            },
            status: JoinRequestStatus::Pending,
            created_at: Timestamp::now_utc(),
            reviewed_by: None,
            reviewed_at: None,
        }
    }

    /// Whether a rejected request may be renewed now
    pub fn cooldown_over(&self, now: Timestamp) -> bool {
        match (&self.status, self.reviewed_at) {
            (JoinRequestStatus::Rejected, Some(at)) => {
                (now.duration_since(at)).whole_seconds() >= JOIN_REQUEST_COOLDOWN_SECONDS
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iso8601_timestamp::Duration;

    #[test]
    fn rejected_waits_a_day_before_asking_again() {
        let now = Timestamp::now_utc();
        let mut request = ServerJoinRequest::pending("s", "u");
        request.status = JoinRequestStatus::Rejected;
        request.reviewed_at = Some(now - Duration::hours(1));
        assert!(!request.cooldown_over(now));
        request.reviewed_at = Some(now - Duration::hours(25));
        assert!(request.cooldown_over(now));
    }

    #[test]
    fn pending_and_approved_have_no_cooldown() {
        let now = Timestamp::now_utc();
        assert!(ServerJoinRequest::pending("s", "u").cooldown_over(now));
    }
}
