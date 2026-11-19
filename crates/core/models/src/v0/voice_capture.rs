#[cfg(feature = "validator")]
use validator::Validate;

auto_derived!(
    /// What a saved piece of a voice room is
    #[derive(Copy, Default)]
    pub enum VoiceCaptureKind {
        /// The last seconds of a room, saved by someone in it
        #[default]
        Clip,
        /// A recorded room that members can hear later
        Episode,
    }

    /// A recording that is running in a voice room
    pub struct VoiceRecordingLive {
        /// Who started it
        pub by: String,
        /// When it started (milliseconds since 1970)
        pub started_at: i64,
        /// Name of the episode
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub title: Option<String>,
    }

    /// Clips and recording in a voice room right now (sent whole as
    /// `VoiceCaptureUpdate` whenever it changes)
    #[derive(Default)]
    pub struct VoiceCaptureState {
        /// Who turned clips on in this room; none while clips are off
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub clips: Option<String>,
        /// The recording that is running, if any
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub recording: Option<VoiceRecordingLive>,
        /// People in the room whose voice may be in a clip or the episode.
        /// Nobody else's voice may be saved.
        #[cfg_attr(feature = "serde", serde(default))]
        pub included: Vec<String>,
    }

    /// What the person asking may still do this month
    #[derive(Default)]
    pub struct VoiceCaptureAllowance {
        /// Length of a clip in seconds
        pub clip_seconds: u32,
        /// Clips this person may still share this month
        pub clips_left: u32,
        /// Clips a person may share per month (theirs, with or without Feather)
        pub clips_per_month: u32,
        /// Seconds this server may still record this month
        pub recording_seconds_left: u64,
        /// Seconds this server may record per month at its Wings level
        pub recording_seconds_per_month: u64,
        /// Longest single episode in seconds
        pub episode_max_seconds: u32,
        /// True when a voice is included only after its owner agreed
        pub opt_in: bool,
    }

    /// State of a room and what the person asking may do
    #[derive(Default)]
    pub struct VoiceCaptureInfo {
        pub state: VoiceCaptureState,
        pub allowance: VoiceCaptureAllowance,
    }

    /// A saved clip or episode
    pub struct VoiceCapture {
        /// Unique id (its time is when it was saved)
        #[cfg_attr(feature = "serde", serde(rename = "_id"))]
        pub id: String,
        pub kind: VoiceCaptureKind,
        /// The voice room it was made in
        pub channel: String,
        /// The server of that room
        pub server: String,
        /// Who saved it
        pub author: String,
        /// The audio file (tag `attachments`)
        pub file: String,
        /// The channel it was shared in
        pub shared_in: String,
        /// The message that carries it
        pub message: String,
        /// Length in seconds
        pub duration: u32,
        /// The people whose voice is in it; each of them agreed and each of
        /// them can delete it
        pub included: Vec<String>,
        /// Name of an episode
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub title: Option<String>,
        /// Removed: the file, the message, the name and the list of people
        /// are gone; only the length is kept for the count of the month
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "crate::if_false", default))]
        pub removed: bool,
    }

    /// Turn clips on or off in a voice room
    pub struct DataVoiceClipsSwitch {
        pub on: bool,
    }

    /// Whether your voice may be in clips and the recording of this room
    pub struct DataVoiceCaptureConsent {
        pub include: bool,
    }

    /// Share a clip
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataVoiceClip {
        /// The uploaded audio file (tag `attachments`, yours, not used yet)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub file: String,
        /// Length in seconds
        pub duration: u32,
        /// Whose voice is in it. Everyone named must have agreed.
        #[cfg_attr(feature = "validator", validate(length(max = 100)))]
        pub included: Vec<String>,
        /// Channel of the same server to share it in (default: the room's
        /// own chat)
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub share_in: Option<String>,
    }

    /// Start recording a voice room
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataVoiceRecordingStart {
        /// Name of the episode (1-80 characters)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 80)))]
        #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none", default))]
        pub title: Option<String>,
    }

    /// Save the recording as an episode
    #[cfg_attr(feature = "validator", derive(Validate))]
    pub struct DataVoiceRecordingFinish {
        /// The uploaded audio file (tag `attachments`, yours, not used yet)
        #[cfg_attr(feature = "validator", validate(length(min = 1, max = 128)))]
        pub file: String,
        /// Length in seconds
        pub duration: u32,
        /// Whose voice is in it. Everyone named must have agreed.
        #[cfg_attr(feature = "validator", validate(length(max = 100)))]
        pub included: Vec<String>,
    }
);
