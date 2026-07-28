//! Deriving the LFS server endpoint from the git remote.
//!
//! This is what lets `ds` have no remote configuration of its own: the object
//! store is wherever the repository already pushes. Rules follow the git-lfs
//! spec and match Gitea's `modules/lfs/endpoint.go`, so a repo that works with
//! `git lfs push` works with `ds push` without extra setup.

use url::Url;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("remote URL is empty")]
    Empty,
    #[error("could not parse remote URL {0:?}")]
    Unparseable(String),
    #[error("unsupported remote scheme {0:?}")]
    UnsupportedScheme(String),
}

/// Derives the LFS endpoint, honouring an explicit `lfs.url` override.
pub fn determine(clone_url: &str, lfs_url: Option<&str>) -> Result<Url, EndpointError> {
    match lfs_url {
        // An explicit lfs.url is used verbatim; no `.git/info/lfs` suffixing.
        Some(u) if !u.is_empty() => normalize(u),
        _ => {
            let mut url = normalize(clone_url)?;
            append_info_lfs(&mut url);
            Ok(url)
        }
    }
}

/// Converts a remote URL into an https base URL.
fn normalize(raw: &str) -> Result<Url, EndpointError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(EndpointError::Empty);
    }

    // Must come first: `Url::parse` would read `host.example:owner/repo.git`
    // as scheme `host.example`, so scp-like syntax never reaches the fallback.
    if !raw.contains("://") {
        return scp_like(raw).ok_or_else(|| EndpointError::Unparseable(raw.to_owned()));
    }

    let parsed = Url::parse(raw).map_err(|_| EndpointError::Unparseable(raw.to_owned()))?;

    match parsed.scheme() {
        "http" | "https" => Ok(strip_trailing_slash(parsed)),
        // LFS speaks HTTP even when git speaks ssh or the git protocol. The
        // scheme cannot be swapped in place (the url crate forbids non-special
        // -> special), so rebuild from host and path, dropping the ssh port and
        // userinfo: the LFS server lives on the web listener, not the ssh one.
        "ssh" | "git+ssh" | "git" => {
            let host = parsed
                .host_str()
                .ok_or_else(|| EndpointError::Unparseable(raw.to_owned()))?;
            let path = parsed.path().trim_start_matches('/');
            Url::parse(&format!("https://{host}/{path}"))
                .map(strip_trailing_slash)
                .map_err(|_| EndpointError::Unparseable(raw.to_owned()))
        }
        other => Err(EndpointError::UnsupportedScheme(other.to_owned())),
    }
}

/// Handles `git@host:owner/repo.git`, which is not a valid URL.
fn scp_like(raw: &str) -> Option<Url> {
    let (userhost, path) = raw.split_once(':')?;
    let host = userhost.rsplit('@').next()?;
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Url::parse(&format!("https://{host}/{}", path.trim_start_matches('/')))
        .ok()
        .map(strip_trailing_slash)
}

fn strip_trailing_slash(mut url: Url) -> Url {
    let trimmed = url.path().trim_end_matches('/').to_owned();
    url.set_path(&trimmed);
    url
}

/// `.../repo.git` gains `/info/lfs`; anything else gains `.git/info/lfs`.
fn append_info_lfs(url: &mut Url) {
    let path = url.path().to_owned();
    let suffixed = if path.ends_with(".git") {
        format!("{path}/info/lfs")
    } else {
        format!("{path}.git/info/lfs")
    };
    url.set_path(&suffixed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(remote: &str) -> String {
        determine(remote, None).unwrap().to_string()
    }

    #[test]
    fn https_remote_ending_in_git() {
        assert_eq!(
            endpoint("https://git.example.com/owner/repo.git"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn https_remote_without_git_suffix() {
        assert_eq!(
            endpoint("https://git.example.com/owner/repo"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn trailing_slash_is_ignored() {
        assert_eq!(
            endpoint("https://git.example.com/owner/repo.git/"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn ssh_remote_becomes_https() {
        assert_eq!(
            endpoint("ssh://git@git.example.com/owner/repo.git"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    /// The ssh port must not leak into the https endpoint — the LFS server is
    /// on the web listener, not the ssh one.
    #[test]
    fn ssh_port_and_user_are_dropped() {
        assert_eq!(
            endpoint("ssh://git@git.example.com:2222/owner/repo.git"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn scp_like_remote_is_supported() {
        assert_eq!(
            endpoint("git@git.example.com:owner/repo.git"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn scp_like_without_user() {
        assert_eq!(
            endpoint("git.example.com:owner/repo.git"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn git_protocol_becomes_https() {
        assert_eq!(
            endpoint("git://git.example.com/owner/repo.git"),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn explicit_lfs_url_wins_and_is_not_suffixed() {
        assert_eq!(
            determine(
                "https://git.example.com/owner/repo.git",
                Some("https://lfs.example.com/custom")
            )
            .unwrap()
            .to_string(),
            "https://lfs.example.com/custom"
        );
    }

    #[test]
    fn empty_lfs_url_falls_back_to_the_clone_url() {
        assert_eq!(
            determine("https://git.example.com/owner/repo.git", Some(""))
                .unwrap()
                .to_string(),
            "https://git.example.com/owner/repo.git/info/lfs"
        );
    }

    #[test]
    fn empty_remote_is_an_error() {
        assert_eq!(determine("", None), Err(EndpointError::Empty));
    }

    #[test]
    fn nested_subgroup_paths_are_preserved() {
        assert_eq!(
            endpoint("https://git.example.com/org/team/repo.git"),
            "https://git.example.com/org/team/repo.git/info/lfs"
        );
    }

    #[test]
    fn host_port_on_https_is_preserved() {
        // Unlike ssh, an explicit https port is where the server really is.
        assert_eq!(
            endpoint("https://git.example.com:3000/owner/repo.git"),
            "https://git.example.com:3000/owner/repo.git/info/lfs"
        );
    }
}
