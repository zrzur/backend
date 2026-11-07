mod model;
mod ops;

pub use model::*;
pub use ops::*;

use std::collections::HashMap;

/// In-memory OAuth storage of the reference database (tests)
#[derive(Default, Debug, Clone)]
pub struct ReferenceOAuth {
    pub clients: HashMap<String, OAuthClient>,
    pub codes: HashMap<String, OAuthCode>,
    pub tokens: HashMap<String, OAuthToken>,
    pub grants: HashMap<String, OAuthGrant>,
    pub installs: HashMap<String, AppInstall>,
}
