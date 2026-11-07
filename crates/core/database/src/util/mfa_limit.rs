//! Two-factor checks: wrong answers are counted per account, and a time code
//! works once. Both need Redis; when it is unreachable the check is skipped,
//! as a broken cache must not lock people out of their accounts.

use redis_kiss::{
    get_connection,
    redis::{ExistenceCheck, SetExpiry, SetOptions},
    AsyncCommands,
};

/// Wrong answers before the account's two-factor checks pause
pub const MFA_FAILURE_LIMIT: i64 = 5;
/// Seconds the wrong answers are remembered, and so how long the pause lasts
pub const MFA_FAILURE_WINDOW: usize = 15 * 60;
/// Seconds a used code is remembered: longer than a code can stay valid
/// (its 30-second step, with one step of drift each way)
const USED_CODE_WINDOW: usize = 120;

fn failures_key(account: &str) -> String {
    format!("mfa:fail:{account}")
}

/// Whether this account has had too many wrong answers
pub async fn mfa_is_paused(account: &str) -> bool {
    let Ok(mut redis) = get_connection().await else {
        return false;
    };
    let count = redis
        .get::<_, Option<i64>>(failures_key(account))
        .await
        .ok()
        .flatten();
    count.unwrap_or(0) >= MFA_FAILURE_LIMIT
}

/// Count one wrong answer. The window starts with the first one, so more
/// wrong answers never make the pause longer.
pub async fn count_mfa_failure(account: &str) {
    let Ok(mut redis) = get_connection().await else {
        return;
    };
    let key = failures_key(account);
    let count: Result<i64, _> = redis.incr(&key, 1).await;
    if matches!(count, Ok(1)) {
        let _: Result<(), _> = redis.expire(&key, MFA_FAILURE_WINDOW).await;
    }
}

/// Forget the wrong answers after a right one
pub async fn clear_mfa_failures(account: &str) {
    if let Ok(mut redis) = get_connection().await {
        let _: Result<(), _> = redis.del(failures_key(account)).await;
    }
}

/// Mark a time code as used; false if it was used already
pub async fn claim_totp_code(account: &str, code: &str) -> bool {
    let Ok(mut redis) = get_connection().await else {
        return true;
    };
    let claimed: Result<Option<String>, _> = redis
        .set_options(
            format!("mfa:used:{account}:{code}"),
            1,
            SetOptions::default()
                .conditional_set(ExistenceCheck::NX)
                .with_expiration(SetExpiry::EX(USED_CODE_WINDOW)),
        )
        .await;
    !matches!(claimed, Ok(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(name: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{name}-{nanos}")
    }

    #[tokio::test]
    async fn a_code_works_once_per_account() {
        let (a, b) = (fresh("a"), fresh("b"));
        assert!(claim_totp_code(&a, "123456").await);
        assert!(!claim_totp_code(&a, "123456").await);
        assert!(claim_totp_code(&a, "654321").await);
        assert!(claim_totp_code(&b, "123456").await);
    }

    #[tokio::test]
    async fn wrong_answers_pause_the_account_until_a_right_one() {
        let account = fresh("account");
        for _ in 0..MFA_FAILURE_LIMIT - 1 {
            count_mfa_failure(&account).await;
        }
        assert!(!mfa_is_paused(&account).await);
        count_mfa_failure(&account).await;
        assert!(mfa_is_paused(&account).await);
        clear_mfa_failures(&account).await;
        assert!(!mfa_is_paused(&account).await);
    }
}
