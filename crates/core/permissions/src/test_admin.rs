//! Administrator: a role permission that gives every right in every channel
//! of the server, whatever the channels' overrides say (owner, 2026-11-09).

use crate::{
    calculate_channel_permissions, calculate_server_permissions, calculate_standing_channel_permissions,
    ChannelPermission, ChannelType, Override, PermissionQuery, RelationshipStatus,
    DEFAULT_PERMISSION_SERVER,
};

const VIEW: u64 = ChannelPermission::ViewChannel as u64;
const ADMIN: u64 = ChannelPermission::Administrator as u64;
const SAFE: u64 = ChannelPermission::GrantAllSafe as u64;

/// A member in a server channel, every input set by the test
#[derive(Clone)]
struct Member {
    server_default: u64,
    server_roles: Vec<Override>,
    channel_default: Override,
    channel_roles: Vec<Override>,
    member: Option<Override>,
    timed_out: bool,
    can_publish: bool,
}

impl Default for Member {
    fn default() -> Self {
        Self {
            server_default: *DEFAULT_PERMISSION_SERVER,
            server_roles: vec![],
            channel_default: Override::default(),
            channel_roles: vec![],
            member: None,
            timed_out: false,
            can_publish: true,
        }
    }
}

fn allow(bits: u64) -> Override {
    Override { allow: bits, deny: 0 }
}

fn deny(bits: u64) -> Override {
    Override { allow: 0, deny: bits }
}

#[async_trait]
impl PermissionQuery for Member {
    async fn are_we_privileged(&mut self) -> bool {
        false
    }
    async fn are_we_a_bot(&mut self) -> bool {
        false
    }
    async fn are_the_users_same(&mut self) -> bool {
        false
    }
    async fn user_relationship(&mut self) -> RelationshipStatus {
        RelationshipStatus::None
    }
    async fn user_is_bot(&mut self) -> bool {
        false
    }
    async fn have_mutual_connection(&mut self) -> bool {
        false
    }
    async fn user_accepts_direct_messages(&mut self) -> bool {
        false
    }
    async fn has_user_written_in_channel(&mut self) -> bool {
        false
    }
    async fn is_user_official(&mut self) -> bool {
        false
    }
    async fn are_we_server_owner(&mut self) -> bool {
        false
    }
    async fn are_we_a_member(&mut self) -> bool {
        true
    }
    async fn get_default_server_permissions(&mut self) -> u64 {
        self.server_default
    }
    async fn get_our_server_role_overrides(&mut self) -> Vec<Override> {
        self.server_roles.clone()
    }
    async fn are_we_timed_out(&mut self) -> bool {
        self.timed_out
    }
    async fn do_we_have_publish_overwrites(&mut self) -> bool {
        self.can_publish
    }
    async fn do_we_have_receive_overwrites(&mut self) -> bool {
        true
    }
    async fn get_channel_type(&mut self) -> ChannelType {
        ChannelType::ServerChannel
    }
    async fn get_default_channel_permissions(&mut self) -> Override {
        self.channel_default.clone()
    }
    async fn get_our_channel_role_overrides(&mut self) -> Vec<Override> {
        self.channel_roles.clone()
    }
    async fn get_our_channel_member_override(&mut self) -> Option<Override> {
        self.member.clone()
    }
    async fn do_we_own_the_channel(&mut self) -> bool {
        false
    }
    async fn are_we_part_of_the_channel(&mut self) -> bool {
        false
    }
    async fn set_recipient_as_user(&mut self) {}
    async fn set_server_from_channel(&mut self) {}
}

async fn channel(mut m: Member) -> u64 {
    calculate_channel_permissions(&mut m).await.into()
}

#[test]
fn administrator_is_a_safe_bit() {
    assert_eq!(ADMIN, 1 << 46);
    assert_eq!(SAFE & ADMIN, ADMIN);
    assert_eq!(*DEFAULT_PERMISSION_SERVER & ADMIN, 0, "never in the defaults");
}

#[tokio::test]
async fn administrator_sees_a_channel_denied_to_everyone() {
    let value = channel(Member {
        server_roles: vec![allow(ADMIN)],
        channel_default: deny(VIEW),
        ..Default::default()
    })
    .await;
    assert_eq!(value, SAFE);
}

#[tokio::test]
async fn administrator_ignores_overrides_for_its_role_and_itself() {
    let value = channel(Member {
        server_roles: vec![allow(ADMIN)],
        channel_default: deny(VIEW),
        channel_roles: vec![deny(SAFE)],
        member: Some(deny(SAFE)),
        ..Default::default()
    })
    .await;
    assert_eq!(value, SAFE);
}

#[tokio::test]
async fn a_higher_role_denying_administrator_takes_it_away() {
    // lowest first: the admin role, then a higher role that denies it
    let value = channel(Member {
        server_roles: vec![allow(ADMIN), deny(ADMIN)],
        channel_default: deny(VIEW),
        ..Default::default()
    })
    .await;
    assert_eq!(value, 0);
}

#[tokio::test]
async fn administrator_in_a_channel_override_gives_nothing() {
    let value = channel(Member {
        channel_default: deny(VIEW),
        channel_roles: vec![allow(ADMIN)],
        ..Default::default()
    })
    .await;
    assert_eq!(value, 0, "a channel can't make someone an administrator");
}

#[tokio::test]
async fn administrator_is_not_restricted_by_timeout() {
    let value = channel(Member {
        server_roles: vec![allow(ADMIN)],
        timed_out: true,
        ..Default::default()
    })
    .await;
    assert_eq!(value, SAFE);
}

#[tokio::test]
async fn server_mute_still_holds_for_an_administrator() {
    let value = channel(Member {
        server_roles: vec![allow(ADMIN)],
        can_publish: false,
        ..Default::default()
    })
    .await;
    assert_eq!(value & ChannelPermission::Speak as u64, 0);
    assert_eq!(value & VIEW, VIEW);
}

#[tokio::test]
async fn administrator_has_every_server_permission() {
    let mut m = Member {
        server_roles: vec![allow(ADMIN)],
        ..Default::default()
    };
    let value: u64 = calculate_server_permissions(&mut m).await.into();
    assert_eq!(value, SAFE);
}

#[tokio::test]
async fn standing_permissions_follow_administrator() {
    let mut m = Member {
        server_roles: vec![allow(ADMIN)],
        channel_default: deny(VIEW),
        ..Default::default()
    };
    let value: u64 = calculate_standing_channel_permissions(&mut m).await.into();
    assert_eq!(value, SAFE);
}

#[tokio::test]
async fn without_administrator_nothing_changes() {
    let value = channel(Member {
        server_roles: vec![allow(ChannelPermission::ManageServer as u64 | ChannelPermission::ManageRole as u64)],
        channel_default: deny(VIEW),
        ..Default::default()
    })
    .await;
    assert_eq!(value, 0);
}

#[tokio::test]
async fn only_an_administrator_can_hand_it_out() {
    use crate::PermissionValue;
    let manager = PermissionValue::from(*DEFAULT_PERMISSION_SERVER | ChannelPermission::ManagePermissions as u64);
    assert!(manager
        .throw_permission_override(None, &allow(ADMIN))
        .await
        .is_err());
    let admin = PermissionValue::from(SAFE);
    assert!(admin.throw_permission_override(None, &allow(ADMIN)).await.is_ok());
}
