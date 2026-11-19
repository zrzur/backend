//! Saved clips and episodes of voice rooms: what a month allows and how one
//! is removed. The record type itself is `revolt_models::v0::VoiceCapture`.

use revolt_config::FeaturesVoiceCapture;
use revolt_models::v0::{VoiceCapture, VoiceCaptureAllowance, VoiceCaptureKind};
use revolt_result::Result;
use ulid::Ulid;

use crate::{Database, Server, User};

/// Days since 1970-01-01 of a calendar date (proleptic Gregorian)
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Calendar date (year, month) of a day count since 1970-01-01
fn civil_month_from_days(days: i64) -> (i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month)
}

/// First millisecond (UTC) of the calendar month `now_ms` is in
pub fn month_start_ms(now_ms: i64) -> i64 {
    const DAY_MS: i64 = 86_400_000;
    let (year, month) = civil_month_from_days(now_ms.div_euclid(DAY_MS));
    days_from_civil(year, month, 1) * DAY_MS
}

/// The smallest id of a given millisecond: ids from then on sort after it
pub fn first_id_at(ms: i64) -> String {
    Ulid::from_parts(ms.max(0) as u64, 0).to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Clips this person shared this month (removed ones count)
pub async fn clips_this_month(db: &Database, user_id: &str) -> Result<u32> {
    let since = first_id_at(month_start_ms(now_ms()));
    Ok(db
        .fetch_voice_captures_since(VoiceCaptureKind::Clip, Some(user_id), None, &since)
        .await?
        .len() as u32)
}

/// Seconds this server recorded this month (removed episodes count)
pub async fn recorded_seconds_this_month(db: &Database, server_id: &str) -> Result<u64> {
    let since = first_id_at(month_start_ms(now_ms()));
    Ok(db
        .fetch_voice_captures_since(VoiceCaptureKind::Episode, None, Some(server_id), &since)
        .await?
        .iter()
        .map(|episode| u64::from(episode.duration))
        .sum())
}

/// What is left of the month, from the numbers alone
pub fn allowance_of(
    settings: &FeaturesVoiceCapture,
    plus: bool,
    wings_level: usize,
    clips_used: u32,
    recorded_seconds: u64,
) -> VoiceCaptureAllowance {
    let clips_per_month = settings.clips_allowed(plus);
    let recording_seconds_per_month = settings.recording_seconds(wings_level);
    VoiceCaptureAllowance {
        clip_seconds: settings.clip_seconds,
        clips_left: clips_per_month.saturating_sub(clips_used),
        clips_per_month,
        recording_seconds_left: recording_seconds_per_month.saturating_sub(recorded_seconds),
        recording_seconds_per_month,
        episode_max_seconds: settings.episode_max_minutes.saturating_mul(60),
        opt_in: settings.consent == revolt_config::VoiceCaptureConsent::OptIn,
    }
}

/// What this person and this server may still do this month
pub async fn voice_capture_allowance(
    db: &Database,
    user: &User,
    server: &Server,
) -> Result<VoiceCaptureAllowance> {
    let settings = revolt_config::config().await.features.voice_capture;
    Ok(allowance_of(
        &settings,
        user.has_plus().await,
        crate::server_supercharge_level(server).await,
        clips_this_month(db, &user.id).await?,
        recorded_seconds_this_month(db, &server.id).await?,
    ))
}

/// Remove a clip or an episode: its message goes (and with it the file),
/// the record keeps only its length for the count of the month
pub async fn remove_voice_capture(db: &Database, capture: &VoiceCapture) -> Result<()> {
    if capture.removed {
        return Ok(());
    }
    if !capture.message.is_empty() {
        match db.fetch_message(&capture.message).await {
            Ok(message) => message.delete(db).await?,
            // the message was deleted already: only the record is left
            Err(error) if matches!(error.error_type, revolt_result::ErrorType::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    db.remove_voice_capture(&capture.id).await
}

/// Remove episodes older than the retention setting. Returns how many went.
pub async fn sweep_old_voice_episodes(db: &Database) -> Result<usize> {
    let days = revolt_config::config()
        .await
        .features
        .voice_capture
        .episode_retention_days;
    if days == 0 {
        return Ok(0);
    }
    let before = first_id_at(now_ms() - i64::from(days) * 86_400_000);
    let old = db.fetch_voice_episodes_before(&before, 100).await?;
    let count = old.len();
    for episode in &old {
        remove_voice_capture(db, episode).await?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_month_starts_on_its_first_day_utc() {
        // 2026-11-19 14:00:00 UTC
        let now = days_from_civil(2026, 11, 19) * 86_400_000 + 14 * 3_600_000;
        assert_eq!(month_start_ms(now), days_from_civil(2026, 11, 1) * 86_400_000);
        // the last millisecond of a leap day belongs to February
        let leap = days_from_civil(2028, 3, 1) * 86_400_000 - 1;
        assert_eq!(month_start_ms(leap), days_from_civil(2028, 2, 1) * 86_400_000);
        // January keeps its year
        let january = days_from_civil(2027, 1, 1) * 86_400_000;
        assert_eq!(month_start_ms(january), january);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn ids_sort_by_their_time() {
        assert!(first_id_at(1_000) < first_id_at(2_000));
        assert!(first_id_at(1_000).as_str() <= Ulid::from_parts(1_000, 7).to_string().as_str());
    }

    #[test]
    fn the_allowance_follows_feather_and_wings() {
        let settings = FeaturesVoiceCapture::default();
        let plain = allowance_of(&settings, false, 0, 3, 0);
        assert_eq!((plain.clips_per_month, plain.clips_left), (10, 7));
        assert_eq!(plain.recording_seconds_per_month, 600);
        assert!(plain.opt_in);

        let feather = allowance_of(&settings, true, 0, 3, 0);
        assert_eq!(feather.clips_left, 97);

        assert_eq!(allowance_of(&settings, false, 1, 0, 3_600).recording_seconds_left, 4 * 3_600);
        assert_eq!(allowance_of(&settings, false, 3, 0, 0).recording_seconds_per_month, 60 * 3_600);
        // a level past the list uses its last entry; used up never goes below zero
        assert_eq!(allowance_of(&settings, false, 9, 99, 999_999).recording_seconds_left, 0);
        assert_eq!(allowance_of(&settings, false, 9, 99, 0).clips_left, 0);
    }

    #[test]
    fn no_trial_means_no_recording_without_wings() {
        let settings = FeaturesVoiceCapture {
            trial_minutes: 0,
            ..Default::default()
        };
        assert_eq!(allowance_of(&settings, false, 0, 0, 0).recording_seconds_left, 0);
    }
}
