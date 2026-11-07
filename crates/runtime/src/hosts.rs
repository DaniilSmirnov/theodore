//! Routes host calls by the PR's host (plan §6.2). Account-wide calls (inbox,
//! viewer, repos) go to the primary host — GitHub.

use std::sync::Arc;

use async_trait::async_trait;
use theodore_domain::{
    Check, CommentId, HostKind, HostRepo, Person, PrDetail, PrRef, PrSummary, RepoRef, Sha,
    ThreadId,
};
use theodore_ports::{
    Capabilities, Changes, Fetched, HostAdapter, HostError, HostResult, InboxSection, MergeMethod,
    MergeStatus, NewPr, ReviewSubmission, Searched, SubmittedReview, TrackedIssue,
};

pub struct Hosts {
    primary: Arc<dyn HostAdapter>,
    gitlab: Option<Arc<dyn HostAdapter>>,
    bitbucket: Option<Arc<dyn HostAdapter>>,
    local: Arc<dyn HostAdapter>,
}

impl Hosts {
    pub fn new(
        primary: Arc<dyn HostAdapter>,
        gitlab: Option<Arc<dyn HostAdapter>>,
        bitbucket: Option<Arc<dyn HostAdapter>>,
        local: Arc<dyn HostAdapter>,
    ) -> Self {
        Self {
            primary,
            gitlab,
            bitbucket,
            local,
        }
    }

    fn for_pr(&self, pr: &PrRef) -> HostResult<&Arc<dyn HostAdapter>> {
        match pr.repo.host {
            HostKind::Gitlab => self.gitlab.as_ref().ok_or(HostError::Unsupported("gitlab")),
            HostKind::Bitbucket => self
                .bitbucket
                .as_ref()
                .ok_or(HostError::Unsupported("bitbucket")),
            HostKind::Local => Ok(&self.local),
            _ => Ok(&self.primary),
        }
    }

    fn for_repo(&self, repo: &RepoRef) -> HostResult<&Arc<dyn HostAdapter>> {
        match repo.host {
            HostKind::Gitlab => self.gitlab.as_ref().ok_or(HostError::Unsupported("gitlab")),
            HostKind::Bitbucket => self
                .bitbucket
                .as_ref()
                .ok_or(HostError::Unsupported("bitbucket")),
            _ => Ok(&self.primary),
        }
    }

    /// `for_repo`, except a local folder's issues are the local adapter's
    /// (none), never a GitHub lookup of its made-up owner/name.
    fn for_issues(&self, repo: &RepoRef) -> HostResult<&Arc<dyn HostAdapter>> {
        match repo.host {
            HostKind::Local => Ok(&self.local),
            _ => self.for_repo(repo),
        }
    }
}

#[async_trait]
impl HostAdapter for Hosts {
    fn kind(&self) -> HostKind {
        self.primary.kind()
    }
    fn capabilities(&self) -> Capabilities {
        self.primary.capabilities()
    }
    async fn viewer(&self) -> HostResult<Person> {
        self.primary.viewer().await
    }
    async fn viewer_for(&self, host: HostKind) -> HostResult<Person> {
        match host {
            HostKind::Gitlab => {
                self.gitlab
                    .as_ref()
                    .ok_or(HostError::Unsupported("gitlab"))?
                    .viewer()
                    .await
            }
            HostKind::Bitbucket => {
                self.bitbucket
                    .as_ref()
                    .ok_or(HostError::Unsupported("bitbucket"))?
                    .viewer()
                    .await
            }
            _ => self.primary.viewer_for(host).await,
        }
    }
    async fn changed_since(&self, cursor: Option<&str>) -> HostResult<Changes> {
        self.primary.changed_since(cursor).await
    }
    async fn search(&self, section: &InboxSection) -> HostResult<Vec<PrSummary>> {
        self.primary.search(section).await
    }
    async fn search_repo(
        &self,
        repo: &RepoRef,
        section: &InboxSection,
    ) -> HostResult<Vec<PrSummary>> {
        self.for_repo(repo)?.search(section).await
    }
    async fn search_checked(
        &self,
        repo: Option<&RepoRef>,
        section: &InboxSection,
    ) -> HostResult<Searched> {
        match repo {
            Some(repo) => self.for_repo(repo)?.search_checked(None, section).await,
            None => self.primary.search_checked(None, section).await,
        }
    }
    async fn list_repos(&self) -> HostResult<Vec<HostRepo>> {
        self.primary.list_repos().await
    }
    async fn get_pr(&self, pr: &PrRef, etag: Option<&str>) -> HostResult<Fetched<PrDetail>> {
        self.for_pr(pr)?.get_pr(pr, etag).await
    }
    async fn find_open_pr(&self, repo: &RepoRef, head: &str) -> HostResult<Option<PrRef>> {
        self.for_repo(repo)?.find_open_pr(repo, head).await
    }
    async fn create_pr(&self, repo: &RepoRef, pr: &NewPr) -> HostResult<PrRef> {
        self.for_repo(repo)?.create_pr(repo, pr).await
    }
    async fn open_issues(&self, repo: &RepoRef) -> HostResult<Vec<TrackedIssue>> {
        self.for_issues(repo)?.open_issues(repo).await
    }
    async fn get_issue(&self, repo: &RepoRef, number: u64) -> HostResult<TrackedIssue> {
        self.for_issues(repo)?.get_issue(repo, number).await
    }
    async fn set_viewed(&self, pr: &PrRef, path: &str, viewed: bool) -> HostResult<()> {
        self.for_pr(pr)?.set_viewed(pr, path, viewed).await
    }
    async fn submit_review(
        &self,
        pr: &PrRef,
        review: &ReviewSubmission,
    ) -> HostResult<SubmittedReview> {
        self.for_pr(pr)?.submit_review(pr, review).await
    }
    async fn find_marker(&self, pr: &PrRef, marker: &str) -> HostResult<Option<CommentId>> {
        self.for_pr(pr)?.find_marker(pr, marker).await
    }
    async fn reply(&self, pr: &PrRef, thread: &ThreadId, body: &str) -> HostResult<CommentId> {
        self.for_pr(pr)?.reply(pr, thread, body).await
    }
    async fn reply_with_marker(
        &self,
        pr: &PrRef,
        thread: &ThreadId,
        body: &str,
        marker: &str,
    ) -> HostResult<CommentId> {
        self.for_pr(pr)?
            .reply_with_marker(pr, thread, body, marker)
            .await
    }
    async fn find_markers(
        &self,
        pr: &PrRef,
        markers: &[String],
    ) -> HostResult<std::collections::HashSet<String>> {
        self.for_pr(pr)?.find_markers(pr, markers).await
    }
    async fn resolve(&self, pr: &PrRef, thread: &ThreadId, resolved: bool) -> HostResult<()> {
        self.for_pr(pr)?.resolve(pr, thread, resolved).await
    }
    async fn edit_comment(&self, pr: &PrRef, comment: &CommentId, body: &str) -> HostResult<()> {
        self.for_pr(pr)?.edit_comment(pr, comment, body).await
    }
    async fn delete_comment(&self, pr: &PrRef, comment: &CommentId) -> HostResult<()> {
        self.for_pr(pr)?.delete_comment(pr, comment).await
    }
    async fn add_reaction(&self, pr: &PrRef, comment: &CommentId, emoji: &str) -> HostResult<()> {
        self.for_pr(pr)?.add_reaction(pr, comment, emoji).await
    }
    async fn remove_reaction(
        &self,
        pr: &PrRef,
        comment: &CommentId,
        emoji: &str,
    ) -> HostResult<()> {
        self.for_pr(pr)?.remove_reaction(pr, comment, emoji).await
    }
    async fn assignable_users(&self, repo: &RepoRef) -> HostResult<Vec<Person>> {
        self.for_repo(repo)?.assignable_users(repo).await
    }
    async fn request_reviewers(
        &self,
        pr: &PrRef,
        users: &[String],
        teams: &[String],
    ) -> HostResult<()> {
        self.for_pr(pr)?.request_reviewers(pr, users, teams).await
    }
    async fn remove_reviewer(&self, pr: &PrRef, user: &str) -> HostResult<()> {
        self.for_pr(pr)?.remove_reviewer(pr, user).await
    }
    async fn mark_ready(&self, pr: &PrRef) -> HostResult<()> {
        self.for_pr(pr)?.mark_ready(pr).await
    }
    async fn convert_to_draft(&self, pr: &PrRef) -> HostResult<()> {
        self.for_pr(pr)?.convert_to_draft(pr).await
    }
    async fn merge_status(&self, pr: &PrRef) -> HostResult<MergeStatus> {
        self.for_pr(pr)?.merge_status(pr).await
    }
    async fn merge(
        &self,
        pr: &PrRef,
        method: MergeMethod,
        title: Option<String>,
        body: Option<String>,
    ) -> HostResult<Sha> {
        self.for_pr(pr)?.merge(pr, method, title, body).await
    }
    async fn enable_auto_merge(&self, pr: &PrRef, method: MergeMethod) -> HostResult<()> {
        self.for_pr(pr)?.enable_auto_merge(pr, method).await
    }
    async fn disable_auto_merge(&self, pr: &PrRef) -> HostResult<()> {
        self.for_pr(pr)?.disable_auto_merge(pr).await
    }
    async fn enqueue(&self, pr: &PrRef) -> HostResult<()> {
        self.for_pr(pr)?.enqueue(pr).await
    }
    async fn update_branch(&self, pr: &PrRef) -> HostResult<()> {
        self.for_pr(pr)?.update_branch(pr).await
    }
    async fn rerun_check(&self, pr: &PrRef, check: &Check) -> HostResult<()> {
        self.for_pr(pr)?.rerun_check(pr, check).await
    }
    async fn check_log(&self, pr: &PrRef, check: &Check) -> HostResult<Option<String>> {
        self.for_pr(pr)?.check_log(pr, check).await
    }
    async fn repo_info(&self, repo: &RepoRef) -> HostResult<HostRepo> {
        self.for_repo(repo)?.repo_info(repo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `HostAdapter` that only tags which fake instance a
    /// `search` landed on, for the `search_repo` routing test below.
    struct TaggedHost(HostKind, std::sync::Mutex<Vec<String>>);

    impl TaggedHost {
        fn new(kind: HostKind) -> Self {
            Self(kind, std::sync::Mutex::new(Vec::new()))
        }
    }

    #[async_trait]
    impl HostAdapter for TaggedHost {
        fn kind(&self) -> HostKind {
            self.0
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        async fn viewer(&self) -> HostResult<Person> {
            self.1.lock().unwrap().push("viewer".into());
            Err(HostError::Unsupported("viewer"))
        }
        async fn changed_since(&self, _cursor: Option<&str>) -> HostResult<Changes> {
            Err(HostError::Unsupported("changed_since"))
        }
        async fn search(&self, section: &InboxSection) -> HostResult<Vec<PrSummary>> {
            if let InboxSection::Query(q) = section {
                self.1.lock().unwrap().push(q.clone());
            }
            Ok(vec![])
        }
        async fn list_repos(&self) -> HostResult<Vec<HostRepo>> {
            Ok(vec![])
        }
        async fn get_pr(&self, _pr: &PrRef, _etag: Option<&str>) -> HostResult<Fetched<PrDetail>> {
            Err(HostError::Unsupported("get_pr"))
        }
        async fn set_viewed(&self, _pr: &PrRef, _path: &str, _viewed: bool) -> HostResult<()> {
            Err(HostError::Unsupported("set_viewed"))
        }
        async fn submit_review(
            &self,
            _pr: &PrRef,
            _review: &ReviewSubmission,
        ) -> HostResult<SubmittedReview> {
            Err(HostError::Unsupported("submit_review"))
        }
        async fn find_marker(&self, _pr: &PrRef, _marker: &str) -> HostResult<Option<CommentId>> {
            Err(HostError::Unsupported("find_marker"))
        }
        async fn reply(
            &self,
            _pr: &PrRef,
            _thread: &ThreadId,
            _body: &str,
        ) -> HostResult<CommentId> {
            Err(HostError::Unsupported("reply"))
        }
        async fn resolve(
            &self,
            _pr: &PrRef,
            _thread: &ThreadId,
            _resolved: bool,
        ) -> HostResult<()> {
            Err(HostError::Unsupported("resolve"))
        }
        async fn edit_comment(
            &self,
            _pr: &PrRef,
            _comment: &CommentId,
            _body: &str,
        ) -> HostResult<()> {
            Err(HostError::Unsupported("edit_comment"))
        }
        async fn delete_comment(&self, _pr: &PrRef, _comment: &CommentId) -> HostResult<()> {
            Err(HostError::Unsupported("delete_comment"))
        }
        async fn open_issues(&self, _repo: &RepoRef) -> HostResult<Vec<TrackedIssue>> {
            self.1.lock().unwrap().push("open_issues".into());
            Ok(vec![])
        }
        async fn get_issue(&self, _repo: &RepoRef, number: u64) -> HostResult<TrackedIssue> {
            self.1.lock().unwrap().push(format!("get_issue {number}"));
            Ok(TrackedIssue {
                number,
                title: "t".into(),
                body: String::new(),
                url: String::new(),
                closed: false,
            })
        }
    }

    /// The names of the `fn`s between `start` and the next line that is
    /// only `}`.
    fn fns_in(source: &str, start: &str) -> std::collections::BTreeSet<String> {
        let from = source.find(start).expect("block present");
        source[from..]
            .lines()
            .skip(1)
            .take_while(|l| *l != "}")
            .filter_map(|l| {
                let l = l.trim_start();
                let rest = l
                    .strip_prefix("async fn ")
                    .or_else(|| l.strip_prefix("fn "))?;
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (!name.is_empty()).then_some(name)
            })
            .collect()
    }

    /// A trait method the router doesn't forward falls through to the
    /// trait's default `Unsupported` for every host (how start-from-issue
    /// broke): each must be forwarded here.
    #[test]
    fn the_router_forwards_every_host_adapter_method() {
        let ports = include_str!("../../ports/src/host.rs");
        let wanted = fns_in(ports, "pub trait HostAdapter: Send + Sync {");
        assert!(wanted.contains("get_issue"), "{wanted:?}");
        let have = fns_in(include_str!("hosts.rs"), "impl HostAdapter for Hosts {");
        let missing: Vec<_> = wanted.difference(&have).collect();
        assert!(missing.is_empty(), "Hosts doesn't forward {missing:?}");
    }

    #[tokio::test]
    async fn issues_go_to_the_repos_own_host() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let gitlab = Arc::new(TaggedHost::new(HostKind::Gitlab));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github.clone(), Some(gitlab.clone()), None, local.clone());

        hosts
            .open_issues(&repo(HostKind::Github, "a"))
            .await
            .unwrap();
        hosts
            .get_issue(&repo(HostKind::Gitlab, "b"), 12)
            .await
            .unwrap();
        hosts
            .open_issues(&repo(HostKind::Local, "c"))
            .await
            .unwrap();
        assert_eq!(github.1.lock().unwrap().as_slice(), ["open_issues"]);
        assert_eq!(gitlab.1.lock().unwrap().as_slice(), ["get_issue 12"]);
        assert_eq!(local.1.lock().unwrap().as_slice(), ["open_issues"]);
    }

    fn repo(host: HostKind, name: &str) -> RepoRef {
        RepoRef {
            host,
            owner: "o".into(),
            name: name.into(),
        }
    }

    #[tokio::test]
    async fn search_checked_routes_by_repo_and_without_one_to_the_primary() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let gitlab = Arc::new(TaggedHost::new(HostKind::Gitlab));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github.clone(), Some(gitlab.clone()), None, local);

        let query = InboxSection::Query("q".into());
        let found = hosts
            .search_checked(Some(&repo(HostKind::Gitlab, "b")), &query)
            .await
            .unwrap();
        assert_eq!(gitlab.1.lock().unwrap().len(), 1);
        assert_eq!(github.1.lock().unwrap().len(), 0);
        assert!(!found.complete, "a raw query isn't trusted by default");

        hosts.search_checked(None, &query).await.unwrap();
        assert_eq!(gitlab.1.lock().unwrap().len(), 1);
        assert_eq!(github.1.lock().unwrap().len(), 1);

        let found = hosts
            .search_checked(None, &InboxSection::Authored)
            .await
            .unwrap();
        assert!(found.complete, "an inbox section is read to its last page");
    }

    #[tokio::test]
    async fn search_repo_routes_gitlab_repos_to_the_gitlab_adapter() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let gitlab = Arc::new(TaggedHost::new(HostKind::Gitlab));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github.clone(), Some(gitlab.clone()), None, local);

        hosts
            .search_repo(
                &repo(HostKind::Gitlab, "b"),
                &InboxSection::Query("q".into()),
            )
            .await
            .unwrap();
        assert_eq!(gitlab.1.lock().unwrap().len(), 1);
        assert_eq!(github.1.lock().unwrap().len(), 0);

        hosts
            .search_repo(
                &repo(HostKind::Github, "a"),
                &InboxSection::Query("q".into()),
            )
            .await
            .unwrap();
        assert_eq!(github.1.lock().unwrap().len(), 1);
        assert_eq!(gitlab.1.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn viewer_for_gitlab_asks_the_gitlab_adapter() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let gitlab = Arc::new(TaggedHost::new(HostKind::Gitlab));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github.clone(), Some(gitlab.clone()), None, local);
        let _ = hosts.viewer_for(HostKind::Gitlab).await;
        assert!(github.1.lock().unwrap().is_empty());
        assert_eq!(gitlab.1.lock().unwrap().as_slice(), ["viewer"]);
    }

    #[tokio::test]
    async fn search_repo_without_a_gitlab_adapter_is_unsupported_for_a_gitlab_repo() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github, None, None, local);

        let err = hosts
            .search_repo(
                &repo(HostKind::Gitlab, "b"),
                &InboxSection::Query("q".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HostError::Unsupported(_)));
    }

    #[tokio::test]
    async fn search_repo_routes_bitbucket_repos_to_the_bitbucket_adapter() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let bitbucket = Arc::new(TaggedHost::new(HostKind::Bitbucket));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github.clone(), None, Some(bitbucket.clone()), local);

        hosts
            .search_repo(
                &repo(HostKind::Bitbucket, "b"),
                &InboxSection::Query("q".into()),
            )
            .await
            .unwrap();
        assert_eq!(bitbucket.1.lock().unwrap().len(), 1);
        assert_eq!(github.1.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn viewer_for_bitbucket_asks_the_bitbucket_adapter() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let bitbucket = Arc::new(TaggedHost::new(HostKind::Bitbucket));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github.clone(), None, Some(bitbucket.clone()), local);
        let _ = hosts.viewer_for(HostKind::Bitbucket).await;
        assert!(github.1.lock().unwrap().is_empty());
        assert_eq!(bitbucket.1.lock().unwrap().as_slice(), ["viewer"]);
    }

    #[tokio::test]
    async fn search_repo_without_a_bitbucket_adapter_is_unsupported_for_a_bitbucket_repo() {
        let github = Arc::new(TaggedHost::new(HostKind::Github));
        let local = Arc::new(TaggedHost::new(HostKind::Local));
        let hosts = Hosts::new(github, None, None, local);

        let err = hosts
            .search_repo(
                &repo(HostKind::Bitbucket, "b"),
                &InboxSection::Query("q".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HostError::Unsupported(_)));
    }
}
