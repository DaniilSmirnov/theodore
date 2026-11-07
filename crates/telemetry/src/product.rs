//! Product events (docs/telemetry.md "Events"): typed, with fixed field
//! names, emitted as `tracing` records tagged `kind="product"` on target
//! `theodore::product`. No code, paths, titles or comment text: every
//! field is a count, a duration, a flag or a value from a fixed set.

use serde::Serialize;

pub const TARGET: &str = "theodore::product";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProductEvent {
    AppStarted {
        version: String,
        channel: String,
    },
    SignedIn,
    /// `account`, `telemetry`, `github`, `agents`, `projects`, `done`.
    OnboardingStep {
        step: String,
    },
    /// `github`, `gitlab`, `bitbucket`, `local`.
    PrOpened {
        host: String,
    },
    GroupingDone {
        ms: u64,
        /// The agent's first answer was valid (no repair round).
        valid_first_try: bool,
        groups: u32,
        hunks: u32,
        agent: String,
        /// How many agent calls the diff was split into.
        chunks: u32,
    },
    GroupingFailed {
        /// An error code (`grouping.invalid_output`, `agent.crashed`, …).
        code: String,
    },
    SkillRun {
        /// A built-in skill's id, or `custom`.
        skill: String,
        agents: u32,
        findings: u32,
        ms: u64,
    },
    /// `pr`, `group`, `hunk`.
    ChatAsked {
        scope: String,
    },
    TaskDelegated {
        agent: String,
    },
    TaskDecided {
        accepted: bool,
    },
    ReviewSubmitted {
        comments: u32,
        /// `approve`, `request_changes`, `comment` (not `event`: that names
        /// the event itself).
        review_event: String,
    },
    /// `merge`, `squash`, `rebase`.
    Merge {
        method: String,
    },
    SpaceCreated {
        mode: String,
    },
    SpaceTurn {
        ms: u64,
        mode: String,
        ok: bool,
    },
    /// `open_pr`, `open_draft_pr`, `push`, `merge_locally`, `keep_branch`.
    SpaceShipped {
        how: String,
    },
    /// The config layer: `global`, `repo`, `local`.
    ConfigSaved {
        r#where: String,
    },
    /// "Report a problem" went through: `issue`, `issue_with_logs`,
    /// `logs_only`. Never the title, the comment or the issue.
    ProblemReported {
        mode: String,
    },
    /// An update is on disk and waits for a restart. The running version
    /// is `app_version`, as on every event.
    UpdateInstalled {
        version: String,
    },
    /// An update check or download failed: `read_only` (the app runs from
    /// the disk image or a translocated copy), `network`, `signature`,
    /// `other`.
    UpdateFailed {
        reason: String,
    },
    /// The first-launch "Move to Applications?" answer: `moved`,
    /// `declined`, `failed`; `from` is `volume`, `translocated`, `folder`.
    MoveToApplications {
        choice: String,
        from: String,
    },
}

impl ProductEvent {
    pub fn name(&self) -> &'static str {
        match self {
            ProductEvent::AppStarted { .. } => "app_started",
            ProductEvent::SignedIn => "signed_in",
            ProductEvent::OnboardingStep { .. } => "onboarding_step",
            ProductEvent::PrOpened { .. } => "pr_opened",
            ProductEvent::GroupingDone { .. } => "grouping_done",
            ProductEvent::GroupingFailed { .. } => "grouping_failed",
            ProductEvent::SkillRun { .. } => "skill_run",
            ProductEvent::ChatAsked { .. } => "chat_asked",
            ProductEvent::TaskDelegated { .. } => "task_delegated",
            ProductEvent::TaskDecided { .. } => "task_decided",
            ProductEvent::ReviewSubmitted { .. } => "review_submitted",
            ProductEvent::Merge { .. } => "merge",
            ProductEvent::SpaceCreated { .. } => "space_created",
            ProductEvent::SpaceTurn { .. } => "space_turn",
            ProductEvent::SpaceShipped { .. } => "space_shipped",
            ProductEvent::ConfigSaved { .. } => "config_saved",
            ProductEvent::ProblemReported { .. } => "problem_reported",
            ProductEvent::UpdateInstalled { .. } => "update_installed",
            ProductEvent::UpdateFailed { .. } => "update_failed",
            ProductEvent::MoveToApplications { .. } => "move_to_applications",
        }
    }

    /// The event's own fields (without `event`).
    pub fn props(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut v = match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        v.remove("event");
        v
    }

    /// Emits the event as a `tracing` record (`kind="product"`).
    pub fn emit(&self) {
        let props = serde_json::Value::Object(self.props()).to_string();
        tracing::info!(
            target: TARGET,
            kind = "product",
            event = self.name(),
            props = %props,
            "{}",
            self.name()
        );
    }

    /// One sample of every event, for the catalog and its tests.
    pub fn samples() -> Vec<ProductEvent> {
        let s = String::new;
        vec![
            ProductEvent::AppStarted {
                version: s(),
                channel: s(),
            },
            ProductEvent::SignedIn,
            ProductEvent::OnboardingStep { step: s() },
            ProductEvent::PrOpened { host: s() },
            ProductEvent::GroupingDone {
                ms: 0,
                valid_first_try: false,
                groups: 0,
                hunks: 0,
                agent: s(),
                chunks: 0,
            },
            ProductEvent::GroupingFailed { code: s() },
            ProductEvent::SkillRun {
                skill: s(),
                agents: 0,
                findings: 0,
                ms: 0,
            },
            ProductEvent::ChatAsked { scope: s() },
            ProductEvent::TaskDelegated { agent: s() },
            ProductEvent::TaskDecided { accepted: false },
            ProductEvent::ReviewSubmitted {
                comments: 0,
                review_event: s(),
            },
            ProductEvent::Merge { method: s() },
            ProductEvent::SpaceCreated { mode: s() },
            ProductEvent::SpaceTurn {
                ms: 0,
                mode: s(),
                ok: false,
            },
            ProductEvent::SpaceShipped { how: s() },
            ProductEvent::ConfigSaved { r#where: s() },
            ProductEvent::ProblemReported { mode: s() },
            ProductEvent::UpdateInstalled { version: s() },
            ProductEvent::UpdateFailed { reason: s() },
            ProductEvent::MoveToApplications {
                choice: s(),
                from: s(),
            },
        ]
    }

    /// Every event and its field names ("What's collected").
    pub fn catalog() -> Vec<theodore_contract::TelemetryEventInfo> {
        Self::samples()
            .iter()
            .map(|e| theodore_contract::TelemetryEventInfo {
                name: e.name().to_string(),
                fields: {
                    let mut f: Vec<String> = e.props().keys().cloned().collect();
                    f.sort();
                    f
                },
            })
            .collect()
    }
}

/// Emits `event`. Shorthand for `event.emit()`.
pub fn product(event: ProductEvent) {
    event.emit();
}

/// What is never sent, one line each ("What's collected").
pub fn never_sent() -> Vec<String> {
    [
        "Code, diffs, file contents or file paths",
        "Pull request titles, descriptions, comments or chat text",
        "Your account id, email, name or host login",
        "Repository names (hashed with a key unique to this install)",
        "Tokens, passwords or other credentials",
        "Debug logs (they stay in the local log file)",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

const ONBOARDING_STEPS: &[&str] = &[
    "account",
    "telemetry",
    "github",
    "agents",
    "projects",
    "done",
];

/// A UI-reported event, if its values are known; anything else is dropped.
pub fn from_ui(event: &theodore_contract::UiEvent) -> Option<ProductEvent> {
    match event {
        theodore_contract::UiEvent::OnboardingStep { step } => ONBOARDING_STEPS
            .contains(&step.as_str())
            .then(|| ProductEvent::OnboardingStep { step: step.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// The wire shape of every event. Changing a field name or adding an
    /// event changes this: update `tests/product_events.json` and
    /// docs/telemetry.md together (`UPDATE_SNAPSHOTS=1` rewrites the file).
    #[test]
    fn product_event_shapes_match_the_snapshot() {
        // Serialized straight from the enum: field order is declaration order.
        let got = serde_json::to_string_pretty(&ProductEvent::samples()).expect("json") + "\n";
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/product_events.json");
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
            std::fs::write(path, &got).expect("write snapshot");
        }
        let want = std::fs::read_to_string(path).expect("snapshot file");
        assert_eq!(got, want);
    }

    #[test]
    fn every_event_has_a_unique_name_matching_its_tag() {
        let mut names = std::collections::BTreeSet::new();
        for e in ProductEvent::samples() {
            let v = serde_json::to_value(&e).unwrap();
            assert_eq!(v["event"], e.name());
            assert!(names.insert(e.name()), "{} twice", e.name());
        }
        assert_eq!(names.len(), ProductEvent::catalog().len());
    }

    #[test]
    fn the_catalog_lists_field_names() {
        let c = ProductEvent::catalog();
        let g = c.iter().find(|e| e.name == "grouping_done").unwrap();
        assert_eq!(
            g.fields,
            [
                "agent",
                "chunks",
                "groups",
                "hunks",
                "ms",
                "valid_first_try"
            ]
        );
        let w = c.iter().find(|e| e.name == "config_saved").unwrap();
        assert_eq!(w.fields, ["where"]);
    }

    #[test]
    fn ui_events_accept_only_known_steps() {
        let step = |s: &str| theodore_contract::UiEvent::OnboardingStep { step: s.into() };
        assert_eq!(
            from_ui(&step("github")),
            Some(ProductEvent::OnboardingStep {
                step: "github".into()
            })
        );
        assert_eq!(from_ui(&step("acme/secret-repo")), None);
    }
}
