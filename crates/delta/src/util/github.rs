//! GitHub: public repositories of a linked account, and giving back the
//! person's token right after use.

use revolt_config::ApiConnectionOAuth;
use revolt_database::{LinkedRepo, LINKED_TOP_REPOS};
use revolt_result::Result;
use serde::Deserialize;

use super::oauth_link::{clean_text, client, get_json, valid_github_login, GITHUB_API};

#[derive(Deserialize, Default)]
pub struct RepoAnswer {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub stargazers_count: u32,
    #[serde(default)]
    pub fork: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub private: bool,
}

/// A repository name: letters, digits, `-`, `_`, `.`
fn valid_repo_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && name != "."
        && name != ".."
}

/// The most starred own public repositories, without forks or archived ones.
/// Links are built by us from the login and name.
pub fn top_repos(login: &str, answer: Vec<RepoAnswer>) -> Vec<LinkedRepo> {
    let mut repos: Vec<LinkedRepo> = answer
        .into_iter()
        .filter(|repo| !repo.fork && !repo.archived && !repo.private && valid_repo_name(&repo.name))
        .map(|repo| LinkedRepo {
            id: repo.id,
            url: format!("https://github.com/{login}/{}", repo.name),
            description: repo
                .description
                .map(|text| clean_text(&text, 140))
                .filter(|text| !text.is_empty()),
            language: repo
                .language
                .map(|text| clean_text(&text, 32))
                .filter(|text| !text.is_empty()),
            stars: repo.stargazers_count,
            name: repo.name,
        })
        .collect();
    repos.sort_by(|a, b| b.stars.cmp(&a.stars).then_with(|| a.name.cmp(&b.name)));
    repos.truncate(LINKED_TOP_REPOS);
    repos
}

/// Read the public repositories of `login`: with the person's token during
/// Finish, with the app's own id and secret on Refresh
pub async fn repos(config: &ApiConnectionOAuth, login: &str, token: Option<&str>) -> Result<Vec<LinkedRepo>> {
    #[cfg(test)]
    {
        let _ = (config, token);
        return Ok(top_repos(login, test_github::repos(login)));
    }

    #[allow(unreachable_code)]
    {
        if !valid_github_login(login) {
            return Ok(vec![]);
        }
        let mut request = client()
            .get(format!("{}/users/{login}/repos", config.api_base_or(GITHUB_API)))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .query(&[("type", "owner"), ("per_page", "100"), ("sort", "pushed")]);
        request = match token {
            Some(token) => request.bearer_auth(token),
            None => request.basic_auth(&config.client_id, Some(&config.client_secret)),
        };
        let answer: Vec<RepoAnswer> = get_json(request).await?;
        Ok(top_repos(login, answer))
    }
}

#[derive(Deserialize)]
struct UserById {
    login: String,
}

/// The login of this GitHub account id now, with the app's own id and
/// secret. `None` when GitHub no longer has the account.
pub async fn login_of(config: &ApiConnectionOAuth, id: &str) -> Result<Option<String>> {
    #[cfg(test)]
    {
        let _ = config;
        return Ok(test_github::login_of(id));
    }

    #[allow(unreachable_code)]
    {
        if !super::oauth_link::valid_numeric_id(id) {
            return Ok(None);
        }
        let answer = client()
            .get(format!("{}/user/{id}", config.api_base_or(GITHUB_API)))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .basic_auth(&config.client_id, Some(&config.client_secret))
            .send()
            .await
            .map_err(|_| revolt_result::create_error!(LinkedServiceFailed))?;
        if answer.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !answer.status().is_success() {
            return Err(revolt_result::create_error!(LinkedServiceFailed));
        }
        let user: UserById = answer
            .json()
            .await
            .map_err(|_| revolt_result::create_error!(LinkedServiceFailed))?;
        Ok(valid_github_login(&user.login).then_some(user.login))
    }
}

/// Give the person's token back to GitHub so it stops working at once.
/// Best effort: it also runs out by itself after 8 hours.
pub async fn revoke(config: &ApiConnectionOAuth, token: &str) {
    #[cfg(test)]
    {
        let _ = (config, token);
        return;
    }

    #[allow(unreachable_code)]
    {
        let _ = client()
            .delete(format!(
                "{}/applications/{}/token",
                config.api_base_or(GITHUB_API),
                config.client_id
            ))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .basic_auth(&config.client_id, Some(&config.client_secret))
            .json(&serde_json::json!({ "access_token": token }))
            .send()
            .await;
    }
}

#[cfg(test)]
pub mod test_github {
    use super::RepoAnswer;

    /// Ids ending in 404 are gone; ids ending in 7 were renamed
    pub fn login_of(id: &str) -> Option<String> {
        if id.ends_with("404") {
            None
        } else if id.ends_with('7') {
            Some(format!("renamed-{id}"))
        } else {
            Some(format!("dev-{id}"))
        }
    }

    pub fn repos(_login: &str) -> Vec<RepoAnswer> {
        vec![
            RepoAnswer { id: 1, name: "small".into(), stargazers_count: 2, ..Default::default() },
            RepoAnswer {
                id: 2,
                name: "popular".into(),
                description: Some("Most stars\u{0007}".into()),
                language: Some("Rust".into()),
                stargazers_count: 120,
                ..Default::default()
            },
            RepoAnswer { id: 3, name: "middle".into(), stargazers_count: 40, ..Default::default() },
            RepoAnswer { id: 4, name: "forked".into(), stargazers_count: 999, fork: true, ..Default::default() },
            RepoAnswer { id: 5, name: "old".into(), stargazers_count: 500, archived: true, ..Default::default() },
            RepoAnswer { id: 6, name: "tiny".into(), stargazers_count: 0, ..Default::default() },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_repos_leave_out_forks_archived_and_odd_names() {
        let mut answer = test_github::repos("dev");
        answer.push(RepoAnswer { id: 7, name: "../evil".into(), stargazers_count: 9999, ..Default::default() });
        let repos = top_repos("dev", answer);
        let names: Vec<_> = repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["popular", "middle", "small", "tiny"]);
        assert_eq!(repos[0].url, "https://github.com/dev/popular");
        assert_eq!(repos[0].description.as_deref(), Some("Most stars"));
        assert_eq!(repos[0].language.as_deref(), Some("Rust"));
        assert_eq!(repos[1].description, None);
    }
}
