//! `ask_user` (docs/spaces.md §5 "Questions"): the agent asks you
//! something mid-turn and the turn waits for the answer.
//!
//! ```text
//! agent ─▶ ask_user ─▶ Space::question set ─▶ card + "<space>: question"
//!                       │
//!   you answer ─────────┼─▶ the answers ─▶ tool result, turn goes on
//!   Stop ───────────────┼─▶ "stopped"
//!   30 min, no answer ──┴─▶ "go on with your best judgment"
//!   (each way: Space::question cleared)
//! ```
//!
//! The wait lives in memory: a restart interrupts the turn as any other
//! (`fail_interrupted_spaces` clears the question).

use std::time::Duration;

use theodore_contract::{
    SpaceAnswerItem, SpaceAnswerRequest, SpaceQuestion, SpaceQuestionItem, SpaceStatus, models,
};
use theodore_ports::{
    ASK_USER_TIMEOUT_MS, NewAction, SpaceToolError, SyncActionCode, UserAnswer, UserQuestion,
    invalid_questions,
};
use tokio::sync::oneshot;

use crate::core::Core;
use crate::error::{AppError, AppResult};

/// A question set waiting for you: where its answers go.
pub(crate) struct PendingQuestion {
    space: String,
    run: String,
    questions: Vec<SpaceQuestionItem>,
    tx: oneshot::Sender<Vec<SpaceAnswerItem>>,
}

/// How a wait for your answer ended.
enum Waited {
    Answered(Vec<SpaceAnswerItem>),
    Stopped,
    TimedOut,
    /// The turn ended while it waited (`end_space_questions`).
    Gone,
}

impl Core {
    /// `ask_user`: shows `questions` in the space and waits for your
    /// answers, the turn's Stop, or `ASK_USER_TIMEOUT_MS`.
    pub async fn space_tool_ask(
        &self,
        space: &str,
        run_id: &str,
        questions: Vec<UserQuestion>,
    ) -> Result<Vec<UserAnswer>, SpaceToolError> {
        self.ask_within(
            space,
            run_id,
            questions,
            Duration::from_millis(ASK_USER_TIMEOUT_MS),
        )
        .await
    }

    pub(crate) async fn ask_within(
        &self,
        space: &str,
        run_id: &str,
        questions: Vec<UserQuestion>,
        timeout: Duration,
    ) -> Result<Vec<UserAnswer>, SpaceToolError> {
        let unavailable = |why: &str| SpaceToolError::Unavailable(why.to_string());
        if let Some(why) = invalid_questions(&questions) {
            return Err(SpaceToolError::Unavailable(why));
        }
        let row = self
            .space_row(space)
            .await
            .map_err(|e| SpaceToolError::Unavailable(e.to_string()))?;
        if row.status != SpaceStatus::Running || row.run.as_deref() != Some(run_id) {
            return Err(unavailable("this turn is over"));
        }
        let cancel = self
            .0
            .runs
            .read()
            .await
            .get(run_id)
            .map(|h| h.cancel.clone())
            .ok_or_else(|| unavailable("this turn is over"))?;
        let items: Vec<SpaceQuestionItem> = questions
            .iter()
            .map(|q| SpaceQuestionItem {
                question: q.question.trim().to_string(),
                options: q.options.iter().map(|o| o.trim().to_string()).collect(),
                multi: q.multi,
            })
            .collect();
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.questions();
            if pending.values().any(|p| p.space == space) {
                return Err(unavailable(
                    "a question is already waiting for the user; ask one set at a time",
                ));
            }
            pending.insert(
                id.clone(),
                PendingQuestion {
                    space: space.to_string(),
                    run: run_id.to_string(),
                    questions: items.clone(),
                    tx,
                },
            );
        }
        let question = SpaceQuestion {
            id: id.clone(),
            run: run_id.to_string(),
            questions: items.clone(),
            asked_at: self.0.deps.clock.now(),
        };
        if let Err(e) = self.patch_question(space, Some(&question)).await {
            self.questions().remove(&id);
            return Err(SpaceToolError::Unavailable(e.to_string()));
        }

        let waited = tokio::select! {
            answers = rx => answers.map_or(Waited::Gone, Waited::Answered),
            _ = cancel.cancelled() => Waited::Stopped,
            _ = tokio::time::sleep(timeout) => Waited::TimedOut,
        };
        // Stop ends the turn too, which drops the wait: either way, stopped.
        let waited = match waited {
            Waited::Gone if cancel.is_cancelled() => Waited::Stopped,
            w => w,
        };
        self.questions().remove(&id);
        // Unless the turn already moved on (it ended and cleared it).
        if self
            .space_row(space)
            .await
            .is_ok_and(|s| s.question.as_ref().is_some_and(|q| q.id == id))
        {
            let _ = self.patch_question(space, None).await;
        }
        match waited {
            Waited::Answered(answers) => Ok(items
                .into_iter()
                .zip(answers)
                .map(|(q, a)| UserAnswer {
                    question: q.question,
                    selected: a.selected,
                    other: a.other.filter(|o| !o.trim().is_empty()),
                })
                .collect()),
            Waited::Stopped => Err(unavailable("the user stopped the turn")),
            Waited::TimedOut => Err(SpaceToolError::Unavailable(format!(
                "no answer within {} minutes; go on with your best judgment and say what you assumed",
                timeout.as_secs() / 60
            ))),
            Waited::Gone => Err(unavailable("this turn is over")),
        }
    }

    /// Your answers to the question card: one per question, each an
    /// option (several where `multi`) or "Other" text.
    pub async fn answer_space_question(&self, req: SpaceAnswerRequest) -> AppResult<()> {
        let mut pending = self.questions();
        let p = pending
            .get(&req.question)
            .filter(|p| p.space == req.space)
            .ok_or_else(|| AppError::NotFound("this question is no longer waiting".into()))?;
        if let Some(why) = invalid_answers(&p.questions, &req.answers) {
            return Err(AppError::Invalid(why));
        }
        let p = pending.remove(&req.question).expect("found above");
        drop(pending);
        p.tx.send(req.answers)
            .map_err(|_| AppError::NotFound("this question is no longer waiting".into()))
    }

    /// The turn `run` waits on your answer.
    pub(crate) fn asking(&self, run: &str) -> bool {
        self.questions().values().any(|p| p.run == run)
    }

    /// The turn `run` ended: questions it still waits on are dropped.
    pub(crate) fn end_space_questions(&self, run: &str) {
        self.questions().retain(|_, p| p.run != run);
    }

    fn questions(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, PendingQuestion>> {
        self.0.questions.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn patch_question(&self, space: &str, q: Option<&SpaceQuestion>) -> AppResult<()> {
        let now = self.0.deps.clock.now();
        let mut data = serde_json::json!({ "question": q, "updated_at": now });
        if q.is_some() {
            data["attention_at"] = serde_json::json!(now);
        }
        self.apply(vec![NewAction {
            model: models::SPACE.into(),
            model_id: space.to_string(),
            action: SyncActionCode::C,
            data: Some(data),
        }])
        .await?;
        Ok(())
    }
}

/// Why `answers` don't answer `questions`, `None` when they do.
fn invalid_answers(questions: &[SpaceQuestionItem], answers: &[SpaceAnswerItem]) -> Option<String> {
    if answers.len() != questions.len() {
        return Some(format!("answer all {} questions", questions.len()));
    }
    for (q, a) in questions.iter().zip(answers) {
        let other = a.other.as_deref().is_some_and(|o| !o.trim().is_empty());
        if a.selected.is_empty() && !other {
            return Some(format!("\"{}\" has no answer", q.question));
        }
        if !q.multi && a.selected.len() + usize::from(other) > 1 {
            return Some(format!("\"{}\" takes one answer", q.question));
        }
        if let Some(bad) = a.selected.iter().find(|s| !q.options.contains(s)) {
            return Some(format!("\"{bad}\" is not an option of \"{}\"", q.question));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(multi: bool) -> SpaceQuestionItem {
        SpaceQuestionItem {
            question: "Which?".into(),
            options: vec!["a".into(), "b".into()],
            multi,
        }
    }

    fn a(selected: &[&str], other: Option<&str>) -> SpaceAnswerItem {
        SpaceAnswerItem {
            selected: selected.iter().map(|s| s.to_string()).collect(),
            other: other.map(str::to_string),
        }
    }

    #[test]
    fn answers_must_fit_their_questions() {
        assert_eq!(invalid_answers(&[q(false)], &[a(&["a"], None)]), None);
        assert_eq!(invalid_answers(&[q(false)], &[a(&[], Some("c"))]), None);
        assert_eq!(invalid_answers(&[q(true)], &[a(&["a", "b"], None)]), None);
        assert!(invalid_answers(&[q(false)], &[]).is_some());
        assert!(invalid_answers(&[q(false)], &[a(&[], Some(" "))]).is_some());
        assert!(invalid_answers(&[q(false)], &[a(&["a", "b"], None)]).is_some());
        assert!(invalid_answers(&[q(false)], &[a(&["a"], Some("c"))]).is_some());
        assert!(invalid_answers(&[q(false)], &[a(&["z"], None)]).is_some());
    }
}
