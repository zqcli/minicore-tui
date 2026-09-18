//! The session lifecycle state machine: creation, open/close/delete/rename,
//! the state/presentation/context read requests, and the catalog generation
//! that owns the `session.list` views (spec §5.3, §6).
//!
//! These are `pub(super)` methods on `App`; the owner, the event router and
//! navigation stay in `app.rs`.

use super::*;

impl App {
    pub(super) fn upsert_session_list(&mut self, session: SessionInfo) {
        // A local lifecycle mutation makes every in-flight catalog response
        // stale, so a late list can never resurrect this old title or a
        // deleted row (spec §3.5).
        self.bump_session_list_generation();
        let mut session = session;
        if self.sessions.deleted.contains(&session.session_id)
            || self.sessions.pending_deletes.contains(&session.session_id)
        {
            return;
        }
        if self.sessions.closed.contains(&session.session_id) {
            session.loaded = false;
        }
        if let Some(existing) = self
            .sessions
            .list
            .iter_mut()
            .find(|existing| existing.session_id == session.session_id)
        {
            *existing = session;
        } else {
            self.sessions.list.push(session);
        }
        self.sessions
            .list
            .sort_by(|left, right| left.session_id.cmp(&right.session_id));
        self.reconcile_session_selection(true);
    }

    pub(super) fn session_action_safety(&self, session_id: &SessionId) -> SessionActionSafety {
        let listed = self
            .sessions
            .list
            .iter()
            .find(|session| &session.session_id == session_id);
        let Some(view) = self.sessions.known.get(session_id) else {
            return if self.sessions.closed.contains(session_id) {
                SessionActionSafety::Safe
            } else if listed.is_some_and(|session| session.loaded) {
                SessionActionSafety::Unknown
            } else {
                SessionActionSafety::Safe
            };
        };

        if view.event_gap {
            return SessionActionSafety::Busy;
        }
        if view.close_verification_unknown || view.latest_state_query.is_some() {
            return SessionActionSafety::Unknown;
        }

        if view.closing
            || view.live.is_some()
            || view.unsaved_loop.is_some()
            || view.needs_result_confirmation()
            || view.is_blocked()
            || view
                .state
                .as_ref()
                .is_some_and(|state| state.status != SessionStatusWire::Idle)
        {
            return SessionActionSafety::Busy;
        }

        let loaded = !self.sessions.closed.contains(session_id)
            && (view.info.loaded || listed.is_some_and(|session| session.loaded));
        if loaded && view.state.is_none() {
            return SessionActionSafety::Unknown;
        }
        if loaded
            && (view.history_read.is_loading()
                || view.history_read.is_reconciling()
                || self.pending_history(session_id))
        {
            return SessionActionSafety::Busy;
        }
        SessionActionSafety::Safe
    }

    pub(super) fn request_session_state_for_action(
        &mut self,
        session_id: &SessionId,
    ) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        if !self.sessions.known.contains_key(session_id) {
            let Some(info) = self
                .sessions
                .list
                .iter()
                .find(|session| &session.session_id == session_id)
                .cloned()
            else {
                return Vec::new();
            };
            self.sessions
                .known
                .insert(session_id.clone(), SessionView::new(info));
        }
        if self
            .sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.latest_state_query.is_some())
            || self.pending_requests.values().any(|request| {
                matches!(
                    request,
                    RequestKind::SessionState { session_id: pending, .. }
                        if pending == session_id
                )
            })
        {
            return Vec::new();
        }
        vec![self.request_session_state(session_id)]
    }

    pub(super) fn session_loaded(&self, session_id: &SessionId) -> Option<bool> {
        if self.sessions.closed.contains(session_id) {
            return Some(false);
        }
        let listed = self
            .sessions
            .list
            .iter()
            .find(|session| &session.session_id == session_id)
            .map(|session| session.loaded)
            .unwrap_or(false);
        self.sessions
            .known
            .get(session_id)
            .map(|view| view.info.loaded || listed)
            .or_else(|| {
                self.sessions
                    .list
                    .iter()
                    .find(|session| &session.session_id == session_id)
                    .map(|session| session.loaded)
            })
    }

    pub(super) fn invalidate_session_state_requests(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.latest_state_query = None;
        }
        self.pending_requests.retain(|_, request| {
            !matches!(
                request,
                RequestKind::SessionState { session_id: pending, .. }
                    if pending == session_id
            )
        });
    }

    pub(super) fn mark_close_verification_unknown(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.closing = false;
            view.close_verification_unknown = true;
            view.steer_state_unconfirmed = true;
            if view
                .state
                .as_ref()
                .is_some_and(|state| state.status == SessionStatusWire::Idle)
            {
                view.state = None;
            }
        }
    }

    pub(super) fn report_unknown_session_state(
        &mut self,
        session_id: &SessionId,
        action: &str,
    ) -> Vec<AppCommand> {
        if let Some(state) = self.session_selector_state_mut() {
            state.error = Some(format!(
                "cannot {action} while session state is unknown; reread it before retrying"
            ));
        }
        self.notice(
            NoticeLevel::Warning,
            format!("Session {session_id} state is unknown; reread it before retrying."),
        );
        self.request_session_state_for_action(session_id)
    }

    pub(super) fn request_session_context(&mut self, session_id: &SessionId) -> Option<AppCommand> {
        if self.reload.is_some() || !self.can_send_requests() {
            return None;
        }
        let poll = self.context_polls.get(session_id).cloned()?;
        let generation = self
            .sessions
            .known
            .get(session_id)
            .map_or(0, |view| view.context_query_generation);
        let key = crate::app::queries::QueryKey::Context {
            session_id: session_id.clone(),
            generation,
        };
        let id = self.next_request_id();
        if self.queries.request_query(key, id) != crate::app::queries::QueryAdmission::Admitted {
            return None;
        }
        self.pending_requests.insert(
            id,
            RequestKind::SessionContext {
                session_id: session_id.clone(),
                generation,
                owner: poll.owner,
            },
        );
        Some(AppCommand::Rpc(OutgoingRequest::session_context(
            id, session_id,
        )))
    }

    pub(super) fn request_session_state(&mut self, session_id: &SessionId) -> AppCommand {
        if self.reload.is_some() {
            return self.request(RequestKind::StaleRead, |id| {
                OutgoingRequest::session_state(id, session_id)
            });
        }
        let query = self.next_state_query;
        self.next_state_query = self
            .next_state_query
            .checked_add(1)
            .expect("session state query space exhausted");
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.latest_state_query = Some(query);
        }
        self.request(
            RequestKind::SessionState {
                session_id: session_id.clone(),
                query,
            },
            |id| OutgoingRequest::session_state(id, session_id),
        )
    }

    pub(super) fn request_session_presentation(
        &mut self,
        session_id: &SessionId,
    ) -> Option<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return None;
        }
        if self.reload.is_some() {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.presentation_refresh_pending = true;
            }
            return None;
        }
        if !self.can_send_requests() {
            return None;
        }
        let view = self.sessions.known.get_mut(session_id)?;
        if view.presentation_pending {
            view.presentation_refresh_pending = true;
            return None;
        }
        view.presentation_pending = true;
        Some(self.request(
            RequestKind::SessionPresentation {
                session_id: session_id.clone(),
            },
            |id| OutgoingRequest::session_presentation(id, session_id),
        ))
    }

    pub(super) fn has_initialized_session_view(view: &SessionView) -> bool {
        view.info.loaded
            || view.state.is_some()
            || view.transcript.complete
            || view.live.is_some()
            || view.unsaved_loop.is_some()
    }

    pub(super) fn activate_existing_session(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        if self.sessions.active.as_ref() != Some(session_id) {
            ui_actions::cancel_scrollbar_drag(self);
            ui_actions::clear_selection(self);
        }
        self.sessions.active = Some(session_id.clone());
        let state_pending = self
            .sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.latest_state_query.is_some());
        let mut commands = if state_pending {
            Vec::new()
        } else {
            vec![self.request_session_state(session_id)]
        };
        if let Some(view) = self.sessions.known.get(session_id) {
            if view.presentation.is_none() && !view.presentation_pending {
                if let Some(command) = self.request_session_presentation(session_id) {
                    commands.push(command);
                }
            }
        }
        let (fetch, reconciling_gap) = {
            let Some(view) = self.sessions.known.get(session_id) else {
                return commands;
            };
            if view.history_read.is_loading() || self.pending_history(session_id) {
                (false, false)
            } else if view.event_gap {
                (true, true)
            } else if !view.transcript.complete {
                (true, false)
            } else {
                (false, false)
            }
        };
        if fetch {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.history_read.begin(if reconciling_gap {
                    HistoryTrigger::Gap
                } else {
                    HistoryTrigger::Refresh
                });
            }
            commands.extend(self.request_history(session_id));
        }
        commands
    }

    pub(super) fn can_activate_existing_session(&self, session_id: &SessionId) -> bool {
        self.sessions.known.get(session_id).is_some_and(|view| {
            view.info.loaded && !view.closing && Self::has_initialized_session_view(view)
        })
    }

    pub(super) fn request_session_id(kind: &RequestKind) -> Option<&str> {
        match kind {
            RequestKind::OpenSession { session_id, .. }
            | RequestKind::CloseSession { session_id }
            | RequestKind::CloseVerifyState { session_id }
            | RequestKind::DeleteSession { session_id }
            | RequestKind::SendTurn { session_id, .. }
            | RequestKind::SteerTurn { session_id, .. }
            | RequestKind::UpdateSession { session_id, .. }
            | RequestKind::RenameSession { session_id }
            | RequestKind::History { session_id, .. }
            | RequestKind::SessionState { session_id, .. }
            | RequestKind::SessionContext { session_id, .. }
            | RequestKind::Compact { session_id, .. }
            | RequestKind::CompactCancel { session_id, .. }
            | RequestKind::SessionPresentation { session_id } => Some(session_id),
            RequestKind::WaitTurn(turn)
            | RequestKind::TurnResult(turn)
            | RequestKind::CancelTurn(turn) => Some(&turn.session_id),
            RequestKind::Reload { .. }
            | RequestKind::StaleRead
            | RequestKind::ReloadModels { .. }
            | RequestKind::ReloadProfiles { .. }
            | RequestKind::ReloadSessions { .. } => None,
            RequestKind::Ping
            | RequestKind::ListModels
            | RequestKind::ListProfiles
            | RequestKind::ListSessions
            | RequestKind::RefreshSessions { .. }
            | RequestKind::CreateSession { .. }
            | RequestKind::Shutdown => None,
        }
    }

    /// A dropped event can invalidate a normal state request that was already
    /// in flight: its response may describe the projection before the gap.
    /// Retire it as a stale read so gap recovery issues a post-gap authority
    /// query instead of replacing the current state projection with old data.
    pub(super) fn fence_pending_session_state(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.latest_state_query = None;
        }
        let ids: Vec<RequestId> = self
            .pending_requests
            .iter()
            .filter_map(|(id, kind)| {
                matches!(
                    kind,
                    RequestKind::SessionState {
                        session_id: pending,
                        ..
                    }
                    | RequestKind::CloseVerifyState {
                        session_id: pending,
                    } if pending == session_id
                )
                .then_some(*id)
            })
            .collect();
        for id in ids {
            let Some(kind) = self.pending_requests.remove(&id) else {
                continue;
            };
            if matches!(kind, RequestKind::CloseVerifyState { .. }) {
                self.mark_close_verification_unknown(session_id);
            }
            self.pending_requests.insert(id, RequestKind::StaleRead);
        }
    }

    /// Sessions left uncalibrated across a reload barrier resume their normal
    /// read chain. The reload itself reads no session view; this is the
    /// generic gap/post-wait recovery a concurrent lifecycle ACK scheduled.
    pub(super) fn resume_uncalibrated_sessions(&mut self) -> Vec<AppCommand> {
        let session_ids: Vec<SessionId> = self
            .sessions
            .known
            .iter()
            .filter_map(|(session_id, view)| {
                (view.event_gap || view.history_read.post_wait_pending())
                    .then_some(session_id.clone())
            })
            .collect();
        let mut commands = Vec::new();
        for session_id in session_ids {
            commands.extend(self.start_gap_reconcile(&session_id));
            commands.extend(self.resume_deferred_reconcile(&session_id));
        }
        commands
    }

    pub(super) fn on_reload_sessions_response(
        &mut self,
        generation: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self
            .reload
            .as_ref()
            .is_none_or(|reload| reload.generation != generation)
        {
            return Vec::new();
        }
        let result = match response.parse_sessions() {
            Ok(result) => result,
            Err(error) => {
                return self.reload_failed(
                    generation,
                    format!("configuration reload session catalog failed: {error}"),
                );
            }
        };
        let mut ids = HashSet::new();
        let sessions: Vec<SessionInfo> = result.sessions;
        if sessions
            .iter()
            .any(|session| !ids.insert(session.session_id.clone()))
        {
            return self.reload_failed(
                generation,
                "configuration reload returned duplicate session IDs",
            );
        }

        if let Some(reload) = self.reload.as_mut() {
            reload.sessions = Some(sessions);
        }
        self.maybe_finish_reload()
    }

    pub(super) fn reload_catalog_error(&self) -> Option<String> {
        let reload = self.reload.as_ref()?;
        let models = reload.models.as_ref()?;
        let profiles = reload.profiles.as_ref()?;
        if models.iter().any(|model| {
            model.id.trim().is_empty()
                || model.model_ref.trim().is_empty()
                || model.supported_reasoning.is_empty()
        }) {
            return Some("configuration reload returned an invalid model catalog".to_owned());
        }
        if profiles
            .iter()
            .any(|profile| profile.id.trim().is_empty() || profile.model.trim().is_empty())
        {
            return Some("configuration reload returned an invalid profile catalog".to_owned());
        }
        for profile in profiles {
            let Some(model) = models
                .iter()
                .find(|model| model.id.as_str() == profile.model.as_str())
            else {
                return Some(format!(
                    "configuration reload profile {} references an unknown model",
                    profile.id
                ));
            };
            if !model.supported_reasoning.contains(&profile.reasoning) {
                return Some(format!(
                    "configuration reload profile {} uses unsupported reasoning",
                    profile.id
                ));
            }
            if !profile.tools.is_empty() && !model.supports_tools {
                return Some(format!(
                    "configuration reload profile {} uses unsupported tools",
                    profile.id
                ));
            }
        }
        None
    }

    pub(super) fn refresh_catalog_seats(&mut self) {
        let profile = self
            .catalogs
            .next_profile
            .take()
            .filter(|id| self.catalogs.profiles.iter().any(|item| &item.id == id))
            .or_else(|| self.catalogs.profiles.first().map(|item| item.id.clone()));
        let profile_model = profile.as_ref().and_then(|profile_id| {
            self.catalogs
                .profiles
                .iter()
                .find(|item| &item.id == profile_id)
                .map(|item| item.model.clone())
        });
        let model = self
            .catalogs
            .next_model
            .take()
            .and_then(|id| {
                self.catalogs
                    .models
                    .iter()
                    .find(|item| {
                        item.id.as_str() == id.as_str() || item.model_ref.as_str() == id.as_str()
                    })
                    .map(|item| item.id.clone())
            })
            .or_else(|| {
                profile_model.as_ref().and_then(|id| {
                    self.catalogs
                        .models
                        .iter()
                        .find(|item| {
                            item.id.as_str() == id.as_str()
                                || item.model_ref.as_str() == id.as_str()
                        })
                        .map(|item| item.id.clone())
                })
            })
            .or_else(|| self.catalogs.models.first().map(|item| item.id.clone()));
        let supported =
            supported_reasoning(&self.catalogs.models, model.as_deref().unwrap_or_default());
        let reasoning = self
            .catalogs
            .next_reasoning
            .filter(|reasoning| supported.contains(reasoning))
            .or_else(|| {
                profile.as_ref().and_then(|profile_id| {
                    self.catalogs
                        .profiles
                        .iter()
                        .find(|item| &item.id == profile_id)
                        .map(|item| item.reasoning)
                        .filter(|reasoning| supported.contains(reasoning))
                })
            })
            .or_else(|| supported.first().copied())
            .or(Some(Reasoning::Auto));
        self.catalogs.next_profile = profile;
        self.catalogs.next_model = model;
        self.catalogs.next_reasoning = reasoning;

        let profile_id = self.catalogs.next_profile.clone().unwrap_or_default();
        let model_id = self.catalogs.next_model.clone().unwrap_or_default();
        let fallback_reasoning = self.catalogs.next_reasoning.unwrap_or(Reasoning::Auto);
        let supported = supported_reasoning(&self.catalogs.models, &model_id);
        let draft_reasoning = supported
            .contains(&fallback_reasoning)
            .then_some(fallback_reasoning)
            .or_else(|| supported.first().copied())
            .unwrap_or(Reasoning::Auto);
        let profile_model = self
            .catalogs
            .profiles
            .iter()
            .find(|item| item.id == profile_id)
            .map(|item| item.model.clone())
            .unwrap_or_else(|| model_id.clone());
        let catalog_models = self.catalogs.models.clone();
        let catalog_profiles = self.catalogs.profiles.clone();
        if let Some(draft) = self.draft_mut() {
            if !draft.submitting {
                if !catalog_profiles.iter().any(|item| item.id == draft.profile) {
                    draft.profile = profile_id;
                }
                if !catalog_models.iter().any(|item| item.id == draft.model) {
                    draft.model = profile_model;
                }
                let draft_supported = supported_reasoning(&catalog_models, &draft.model);
                if !draft_supported.contains(&draft.reasoning) {
                    draft.reasoning = draft_supported.first().copied().unwrap_or(draft_reasoning);
                }
            }
        }
    }

    /// Retires the current loop for event routing without discarding a wait
    /// that may still complete before the new session.open response.
    pub(super) fn retire_reopened_session(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            let retired = view
                .live
                .as_ref()
                .and_then(|live| live.reference.clone())
                .or_else(|| view.unsaved_loop.as_ref().map(|loop_| loop_.turn.clone()))
                .or_else(|| view.last_result.as_ref().map(|result| result.turn.clone()));
            if retired.is_some() {
                view.retired_loop = retired;
            }
            view.latest_state_query = None;
        }
    }

    /// Invalidates only requests belonging to an explicitly reopened session
    /// after the new open response has been accepted. The single retired loop
    /// fence blocks already-buffered old events without retaining an unbounded
    /// registry.
    pub(super) fn invalidate_reopened_session(&mut self, session_id: &SessionId) {
        // Keep retired request ids registered as StaleRead so their late
        // responses still release the read slot they own. Dropping the ids
        // here would make the response look unknown and leak capacity.
        self.retire_session_operations(session_id);
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.history_query_generation = view
                .history_query_generation
                .checked_add(1)
                .expect("history query generations exhausted");
        }
        self.retire_reopened_session(session_id);
    }

    /// Retires operations that belong to a session which is no longer the
    /// current loaded view. Exact wait/result requests remain registered so a
    /// late authoritative outcome can still be retained; all other responses
    /// are consumed as stale and their read slot is released normally.
    pub(super) fn retire_session_operations(&mut self, session_id: &SessionId) {
        let stale_ids: Vec<RequestId> = self
            .pending_requests
            .iter()
            .filter_map(|(id, kind)| {
                let belongs = Self::request_session_id(kind) == Some(session_id.as_str());
                let keep_exact_turn =
                    matches!(kind, RequestKind::WaitTurn(_) | RequestKind::TurnResult(_));
                (belongs && !keep_exact_turn).then_some(*id)
            })
            .collect();
        let stale_submissions: Vec<LocalSubmissionId> = stale_ids
            .iter()
            .filter_map(|id| match self.pending_requests.get(id) {
                Some(RequestKind::SendTurn {
                    local_submission, ..
                }) => Some(*local_submission),
                _ => None,
            })
            .collect();
        for local_submission in stale_submissions {
            self.submissions.remove(&local_submission);
        }
        for id in stale_ids {
            self.pending_requests.insert(id, RequestKind::StaleRead);
        }
        self.context_polls.remove(session_id);
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.context_query_generation = view
                .context_query_generation
                .checked_add(1)
                .expect("context query generations exhausted");
            view.manual_compact = None;
        }
    }

    pub(super) fn mark_session_uncalibrated(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            let was_uncalibrated = view.event_gap;
            Self::mark_history_unconfirmed(view);
            view.latest_state_query = None;
            view.state = None;
            view.close_verification_unknown = true;
            view.steer_state_unconfirmed = true;
            if !was_uncalibrated {
                view.gap_revision = view.gap_revision.wrapping_add(1);
            }
        }
    }

    /// Issues a session catalog request while recording the current catalog
    /// generation, so a late response can be recognized as stale.
    pub(super) fn request_session_catalog(
        &mut self,
        kind: RequestKind,
        build: impl FnOnce(RequestId) -> OutgoingRequest,
    ) -> AppCommand {
        let command = self.request(kind, build);
        if let AppCommand::Rpc(request) = &command {
            let issued = self.catalogs.session_list_generation;
            self.session_list_requests.insert(request.id, issued);
        }
        command
    }

    /// Whether a session-list response may be applied: it must have been
    /// issued at the current catalog generation. A stale response is dropped
    /// and its caller re-issues a fresh list request.
    pub(super) fn session_catalog_is_current(&mut self, id: RequestId) -> bool {
        match self.session_list_requests.remove(&id) {
            Some(issued) => issued == self.catalogs.session_list_generation,
            None => true,
        }
    }

    /// A local catalog mutation invalidates every in-flight `session.list`
    /// response (spec §3.5).
    pub(super) fn bump_session_list_generation(&mut self) {
        self.catalogs.session_list_generation = self
            .catalogs
            .session_list_generation
            .checked_add(1)
            .expect("session catalog generations exhausted");
    }

    /// Re-issues a plain catalog list after a stale response was discarded.
    pub(super) fn refresh_session_catalog(&mut self) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        vec![
            self.request_session_catalog(RequestKind::ListSessions, OutgoingRequest::list_sessions),
        ]
    }

    pub(super) fn create_session(
        &mut self,
        workspace: &str,
        profile: Option<&str>,
        model: Option<&str>,
        reasoning: Option<Reasoning>,
        title: Option<&str>,
    ) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.has_pending_lifecycle_request() {
            return Vec::new();
        }
        vec![
            self.request(RequestKind::CreateSession { draft: u64::MAX }, |id| {
                OutgoingRequest::session_create(id, workspace, profile, model, reasoning, title)
            }),
        ]
    }

    pub(super) fn open_session(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.has_pending_lifecycle_request() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        if self.pending_open_or_history(session_id)
            || self
                .sessions
                .known
                .get(session_id)
                .is_some_and(|view| view.closing)
        {
            return Vec::new();
        }
        if self.sessions.active.as_ref() != Some(session_id) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        if self.can_activate_existing_session(session_id) {
            return self.activate_existing_session(session_id);
        }
        // Establish the lifecycle fence before the new request leaves the
        // reducer. Old notifications can arrive before session.open responds.
        let retired_loop_on_failure = self
            .sessions
            .known
            .get(session_id)
            .and_then(|view| view.retired_loop.clone());
        self.retire_reopened_session(session_id);
        vec![self.request(
            RequestKind::OpenSession {
                session_id: session_id.clone(),
                previous_retired_loop: retired_loop_on_failure,
            },
            |id| OutgoingRequest::session_open(id, session_id),
        )]
    }

    pub(super) fn refresh_sessions(&mut self) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(
                NoticeLevel::Info,
                "configuration reload is already refreshing the catalog",
            );
            return Vec::new();
        }
        if self.session_panel_busy() {
            return Vec::new();
        }
        if !self
            .session_selector_state()
            .is_some_and(|state| matches!(&state.mode, SessionPanelMode::Browse))
        {
            return Vec::new();
        }
        if self
            .pending_requests
            .values()
            .any(|request| matches!(request, RequestKind::RefreshSessions { .. }))
        {
            return Vec::new();
        }
        let selected_session_id = self.selected_session_id();
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
        }
        vec![self.request_session_catalog(
            RequestKind::RefreshSessions {
                selected_session_id,
            },
            OutgoingRequest::list_sessions,
        )]
    }

    pub(super) fn close_session(
        &mut self,
        session_id: &SessionId,
        confirm: bool,
    ) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.has_pending_lifecycle_request() {
            return Vec::new();
        }
        if self.pending_requests.values().any(|request| {
            matches!(
                request,
                RequestKind::CloseSession { session_id: pending }
                    | RequestKind::CloseVerifyState { session_id: pending }
                    if pending == session_id
            )
        }) {
            return Vec::new();
        }
        match self.session_loaded(session_id) {
            Some(true) => {}
            Some(false) => {
                self.notice(NoticeLevel::Info, "Session is already closed.");
                return Vec::new();
            }
            None => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("Session {session_id} is not available for closing."),
                );
                return Vec::new();
            }
        }
        let (is_blocked, has_unsaved, has_unconfirmed, is_active) =
            match self.sessions.known.get(session_id) {
                Some(view) => (
                    view.is_blocked(),
                    view.unsaved_loop.is_some(),
                    view.needs_result_confirmation(),
                    view.live.is_some()
                        || view
                            .state
                            .as_ref()
                            .is_some_and(|state| state.status != SessionStatusWire::Idle),
                ),
                None => (false, false, false, false),
            };
        if (is_blocked || has_unsaved || has_unconfirmed || is_active) && !confirm {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "Session {session_id} has active or unsaved/blocked state. Type '/close confirm' to proceed."
                ),
            );
            return Vec::new();
        }
        if self
            .sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.event_gap)
        {
            if let Some(state) = self.session_selector_state_mut() {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    state.error =
                        Some("cannot close while history reconciliation is incomplete".to_owned());
                }
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "Cannot close session {session_id} until history reconciliation completes."
                ),
            );
            return Vec::new();
        }
        let history_pending = self.pending_history(session_id);
        let history_incomplete = self.session_loaded(session_id) == Some(true)
            && (history_pending
                || self.sessions.known.get(session_id).is_some_and(|view| {
                    view.history_read.is_loading() || view.history_read.is_reconciling()
                }));
        if history_incomplete {
            if let Some(state) = self.session_selector_state_mut() {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    state.error = Some("cannot close while history is incomplete".to_owned());
                }
            }
            self.notice(
                NoticeLevel::Warning,
                format!("Cannot close session {session_id} until history completes."),
            );
            return Vec::new();
        }
        if matches!(
            self.session_action_safety(session_id),
            SessionActionSafety::Unknown
        ) {
            return self.report_unknown_session_state(session_id, "close");
        }
        if self.sessions.active.as_deref() == Some(session_id.as_str()) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        let mut commands = Vec::new();
        let reference = if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.closing = true;
            view.live.as_ref().and_then(|l| l.reference.clone())
        } else {
            None
        };
        if let Some(reference) = reference {
            if let Some(command) = self.request_wait(reference) {
                commands.push(command);
            }
        }
        commands.push(self.request(
            RequestKind::CloseSession {
                session_id: session_id.clone(),
            },
            |id| OutgoingRequest::session_close(id, session_id),
        ));
        commands
    }

    pub(super) fn mark_session_closed(&mut self, session_id: &SessionId) {
        self.sessions.closed.insert(session_id.clone());
        self.invalidate_session_state_requests(session_id);
        self.retire_session_operations(session_id);
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.closing = false;
            view.close_verification_unknown = false;
            view.steer_state_unconfirmed = false;
            view.info.loaded = false;
        }
        if let Some(info) = self
            .sessions
            .known
            .get(session_id)
            .map(|view| view.info.clone())
        {
            self.upsert_session_list(info);
        }
        self.retire_reopened_session(session_id);
        if self.sessions.active.as_deref() == Some(session_id.as_str()) {
            ui_actions::cancel_scrollbar_drag(self);
            ui_actions::clear_selection(self);
            self.sessions.active = None;
        }
    }

    pub(super) fn on_close_session_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id) {
            return Vec::new();
        }
        match response.parse_close() {
            Ok(_) => {
                self.mark_session_closed(session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        match &state.mode {
                            SessionPanelMode::ConfirmCloseForDelete => {
                                state.mode = SessionPanelMode::ConfirmDelete {
                                    choice: SessionConfirmChoice::Cancel,
                                    submitting: false,
                                };
                                state.error = None;
                            }
                            SessionPanelMode::ConfirmClose => {
                                state.mode = SessionPanelMode::Browse;
                                state.error = None;
                            }
                            _ => {}
                        }
                    }
                }
                self.reconcile_session_selection(true);
                self.notice(NoticeLevel::Info, format!("Session {session_id} closed."));
                self.maybe_finish_reload()
            }
            Err(RpcResponseError::Agent(_error)) => {
                // MIG-146: close returns error, perform a single read check of session state.
                // Do not retry indefinitely.
                self.invalidate_session_state_requests(session_id);
                if self.reload.is_some() {
                    self.mark_close_verification_unknown(session_id);
                    self.notice(
                        NoticeLevel::Warning,
                        format!(
                            "Session {session_id} close verification is deferred until configuration reload finishes"
                        ),
                    );
                    return self.maybe_finish_reload();
                }
                vec![self.request(
                    RequestKind::CloseVerifyState {
                        session_id: session_id.clone(),
                    },
                    |id| OutgoingRequest::session_state(id, session_id),
                )]
            }
            Err(error) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Error,
                    format!("Failed to close session {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    pub(super) fn on_close_verify_state_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id) {
            return Vec::new();
        }
        match response.parse_session_state() {
            Ok(state) if state.session_id == *session_id => {
                self.apply_session_state(&state, None, SessionStateSource::CloseVerifyResponse);
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    view.closing = false;
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification: status is {:?}; unload not confirmed",
                        state.status
                    ),
                );
            }
            Ok(_) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification returned another session; state is unknown"
                    ),
                );
            }
            Err(RpcResponseError::Agent(error))
                if error.code == crate::protocol::SESSION_NOT_LOADED =>
            {
                self.mark_session_closed(session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        match &state.mode {
                            SessionPanelMode::ConfirmCloseForDelete => {
                                state.mode = SessionPanelMode::ConfirmDelete {
                                    choice: SessionConfirmChoice::Cancel,
                                    submitting: false,
                                };
                                state.error = None;
                            }
                            SessionPanelMode::ConfirmClose => {
                                state.mode = SessionPanelMode::Browse;
                                state.error = None;
                            }
                            _ => {}
                        }
                    }
                }
                self.notice(
                    NoticeLevel::Info,
                    format!("Session {session_id} was confirmed closed."),
                );
            }
            Err(error) => {
                self.mark_close_verification_unknown(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "Session {session_id} close verification is unknown; result/state retained: {error}"
                    ),
                );
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        state.mode = SessionPanelMode::Browse;
                        state.error = Some(error.to_string());
                    }
                }
            }
        }
        self.reconcile_session_selection(true);
        self.maybe_finish_reload()
    }

    pub(super) fn delete_session(
        &mut self,
        session_id: &SessionId,
        confirm: bool,
    ) -> Vec<AppCommand> {
        if !self.guard_ready() {
            return Vec::new();
        }
        if self.reload.is_some() {
            self.notice(NoticeLevel::Info, "wait for configuration reload to finish");
            return Vec::new();
        }
        if self.has_pending_lifecycle_request() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
            || self.pending_requests.values().any(|request| {
                matches!(request, RequestKind::DeleteSession { session_id: pending } if pending == session_id)
            })
        {
            return Vec::new();
        }
        if !confirm {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "Deleting session {session_id} is permanent. Type '/delete confirm' to proceed."
                ),
            );
            return Vec::new();
        }
        let Some(loaded) = self.session_loaded(session_id) else {
            self.notice(
                NoticeLevel::Warning,
                format!("Session {session_id} is not available for deletion."),
            );
            return Vec::new();
        };
        if !matches!(
            self.session_action_safety(session_id),
            SessionActionSafety::Safe
        ) {
            if matches!(
                self.session_action_safety(session_id),
                SessionActionSafety::Unknown
            ) {
                return self.report_unknown_session_state(session_id, "delete");
            }
            self.notice(
                NoticeLevel::Warning,
                format!("Session {session_id} is busy or its result is unconfirmed; deletion is blocked."),
            );
            return Vec::new();
        }
        if loaded {
            if let Dock::SessionSelector(state) = &mut self.dock {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    state.mode = SessionPanelMode::ConfirmCloseForDelete;
                }
            }
            self.notice(
                NoticeLevel::Warning,
                format!("Close session {session_id} before deleting it."),
            );
            return Vec::new();
        }
        if self.sessions.active.as_deref() == Some(session_id.as_str()) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        self.sessions.pending_deletes.insert(session_id.clone());
        vec![self.request(
            RequestKind::DeleteSession {
                session_id: session_id.clone(),
            },
            |id| OutgoingRequest::session_delete(id, session_id),
        )]
    }

    pub(super) fn on_delete_session_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id) {
            return Vec::new();
        }
        match response.parse_delete() {
            Ok(_) => {
                self.pending_requests.retain(|_, request| {
                    Self::request_session_id(request) != Some(session_id.as_str())
                });
                self.mouse_down = None;
                self.panel_click = None;
                self.sessions.pending_deletes.remove(session_id);
                self.sessions.deleted.insert(session_id.clone());
                self.sessions.closed.remove(session_id);
                self.sessions.known.remove(session_id);
                self.sessions.list.retain(|s| &s.session_id != session_id);
                self.bump_session_list_generation();
                if self.sessions.active.as_deref() == Some(session_id.as_str()) {
                    ui_actions::cancel_scrollbar_drag(self);
                    ui_actions::clear_selection(self);
                    self.sessions.active = None;
                }
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        state.mode = SessionPanelMode::Browse;
                        state.error = None;
                    }
                }
                self.reconcile_session_selection(true);
                self.notice(NoticeLevel::Info, format!("Session {session_id} deleted."));
                self.maybe_finish_reload()
            }
            Err(error) => {
                self.sessions.pending_deletes.remove(session_id);
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if matches!(&state.mode, SessionPanelMode::ConfirmDelete { .. }) {
                            state.mode = SessionPanelMode::ConfirmDelete {
                                choice: SessionConfirmChoice::Cancel,
                                submitting: false,
                            };
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Error,
                    format!("Failed to delete session {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    pub(super) fn on_session_response(
        &mut self,
        session_id: SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.sessions.deleted.contains(&session_id)
            || self.sessions.pending_deletes.contains(&session_id)
        {
            return Vec::new();
        }
        let session = match response.parse_session() {
            Ok(result) => result.session,
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    format!("failed to parse session {session_id}: {error}"),
                );
                return Vec::new();
            }
        };
        if session.session_id != session_id {
            self.notice(
                NoticeLevel::Error,
                format!("session response does not match requested session {session_id}"),
            );
            return Vec::new();
        }
        self.sessions.closed.remove(&session_id);
        let mut commands = Vec::new();
        match self.sessions.known.get_mut(&session_id) {
            Some(view) => {
                view.info = session;
                view.completed_steers.clear();
            }
            None => {
                self.sessions
                    .known
                    .insert(session_id.clone(), SessionView::new(session));
            }
        }
        let listed_info = self
            .sessions
            .known
            .get(&session_id)
            .map(|view| view.info.clone());
        if let Some(info) = listed_info {
            self.upsert_session_list(info);
        }
        if self.sessions.active.as_ref() != Some(&session_id) {
            ui_actions::cancel_scrollbar_drag(self);
        }
        ui_actions::clear_selection(self);
        self.sessions.active = Some(session_id.clone());

        if self.reload.is_some() {
            // The lifecycle ACK crossed the reload boundary. The response
            // supplies metadata only; state still needs fresh post-reload
            // authority while the existing history window remains intact.
            self.mark_session_uncalibrated(&session_id);
            return Vec::new();
        }

        commands.push(self.request_session_state(&session_id));
        if let Some(command) = self.request_session_presentation(&session_id) {
            commands.push(command);
        }

        let (fetch, reconciling_gap) = {
            let Some(view) = self.sessions.known.get(&session_id) else {
                return commands;
            };
            if view.history_read.is_loading() {
                (false, false)
            } else if view.event_gap {
                (true, true)
            } else if !view.transcript.complete {
                (true, false)
            } else {
                (false, false)
            }
        };
        if fetch {
            if let Some(view) = self.sessions.known.get_mut(&session_id) {
                view.history_read.begin(if reconciling_gap {
                    HistoryTrigger::Gap
                } else {
                    HistoryTrigger::Refresh
                });
            }
            commands.extend(self.request_history(&session_id));
        }
        commands
    }

    pub(super) fn on_refresh_sessions_response(
        &mut self,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let result = match response.parse_sessions() {
            Ok(result) => result,
            Err(error) => {
                if let Some(state) = self.session_selector_state_mut() {
                    state.error = Some(format!("refresh failed: {error}"));
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session list refresh failed: {error}"),
                );
                return Vec::new();
            }
        };
        let mut visible = Vec::with_capacity(result.sessions.len());
        for mut session in result.sessions {
            let session_id = session.session_id.clone();
            if self.sessions.closed.contains(&session_id) {
                session.loaded = false;
            }
            if let Some(view) = self.sessions.known.get_mut(&session_id) {
                view.info = session.clone();
                if !session.loaded {
                    view.latest_state_query = None;
                }
            } else {
                self.sessions
                    .known
                    .insert(session_id.clone(), SessionView::new(session.clone()));
            }
            visible.push(session);
        }
        self.sessions.list = visible;
        self.reconcile_session_selection(true);
        if let Some(state) = self.session_selector_state_mut() {
            state.error = None;
        }
        Vec::new()
    }

    pub(super) fn on_rename_session_response(
        &mut self,
        session_id: SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(&session_id)
            || self.sessions.pending_deletes.contains(&session_id)
        {
            return Vec::new();
        }
        let parsed = response.parse_session_rename();
        let session = match parsed {
            Ok(result) => result.session,
            Err(error) => {
                if let Dock::SessionSelector(state) = &mut self.dock {
                    if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                        if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                            *submitting = false;
                        }
                        state.error = Some(error.to_string());
                    }
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.rename failed for {session_id}: {error}"),
                );
                return Vec::new();
            }
        };
        if session.session_id != session_id {
            let error =
                format!("session.rename response does not match requested session {session_id}");
            if let Dock::SessionSelector(state) = &mut self.dock {
                if state.selected_session_id.as_deref() == Some(session_id.as_str()) {
                    if let SessionPanelMode::Rename { submitting, .. } = &mut state.mode {
                        *submitting = false;
                    }
                    state.error = Some(error.clone());
                }
            }
            self.notice(NoticeLevel::Warning, error);
            return Vec::new();
        }
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            view.info = session.clone();
        } else {
            self.sessions
                .known
                .insert(session_id.clone(), SessionView::new(session.clone()));
        }
        if self.reload.is_some() {
            self.mark_session_uncalibrated(&session_id);
        }
        self.upsert_session_list(session);
        let mut returned_to_browse = false;
        if let Dock::SessionSelector(state) = &mut self.dock {
            if state.selected_session_id.as_deref() == Some(session_id.as_str())
                && matches!(&state.mode, SessionPanelMode::Rename { .. })
            {
                state.mode = SessionPanelMode::Browse;
                state.error = None;
                returned_to_browse = true;
            }
        }
        if returned_to_browse {
            self.reconcile_session_selection(true);
        }
        self.notice(NoticeLevel::Info, format!("Session {session_id} renamed."));
        Vec::new()
    }

    pub(super) fn on_open_response(
        &mut self,
        session_id: SessionId,
        previous_retired_loop: Option<TurnRef>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(&session_id)
            || self.sessions.pending_deletes.contains(&session_id)
        {
            return Vec::new();
        }
        let parsed = response.parse_session();
        if let Err(error) = &parsed {
            if let Some(retired_loop) = previous_retired_loop.clone() {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.retired_loop = Some(retired_loop);
                }
            }
            let message = match error {
                RpcResponseError::Agent(error) if error.code == crate::protocol::STORE_ERROR => {
                    "Unable to open this session. Its data may be unavailable, invalid, or from an unsupported format.".to_owned()
                }
                _ => format!("session.open failed: {error}"),
            };
            if let Dock::SessionSelector(state) = &mut self.dock {
                state.error = Some(message);
            } else {
                self.notice(NoticeLevel::Error, message);
            }
            return Vec::new();
        }
        if parsed
            .as_ref()
            .is_ok_and(|result| result.session.session_id != session_id)
        {
            if let Some(retired_loop) = previous_retired_loop {
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    view.retired_loop = Some(retired_loop);
                }
            }
            let message =
                format!("session.open response does not match requested session {session_id}");
            if let Dock::SessionSelector(state) = &mut self.dock {
                state.error = Some(message);
            } else {
                self.notice(NoticeLevel::Error, message);
            }
            return Vec::new();
        }
        // Reopen is a lifecycle boundary. Retire old request ids before
        // rebuilding the view so late responses cannot mutate the new load.
        self.invalidate_reopened_session(&session_id);
        self.context_polls.remove(&session_id);
        self.submissions.retain(|_, submission| {
            submission
                .preparation
                .as_ref()
                .is_none_or(|operation| operation.session_id != session_id)
        });
        let retained_after_reopen = previous_retired_loop
            .as_ref()
            .and_then(|turn| self.retained_results.get(turn))
            .cloned();
        if let Some(view) = self.sessions.known.get_mut(&session_id) {
            // Rebuild from history offset 0; never compare the new total with
            // the old local projection.
            let preserving_gap = view.event_gap;
            view.transcript.clear_blocks();
            view.read_page = None;
            view.history_read.finish();
            view.history_read.take_pending();
            if preserving_gap {
                view.history_read.defer(HistoryTrigger::Gap);
            }
            view.closing = false;
            view.live = None;
            view.unsaved_loop = None;
            view.last_result = retained_after_reopen;
            view.usage_projection = crate::state::session::UsageProjection::default();
            view.last_request = None;
            view.config_update = None;
            view.state = None;
            view.context = None;
            view.context_query_generation = view
                .context_query_generation
                .checked_add(1)
                .expect("context query generations exhausted");
            view.manual_compact = None;
            view.session_epoch = view
                .session_epoch
                .checked_add(1)
                .expect("session epochs exhausted");
            view.presentation = None;
            view.presentation_pending = false;
            view.presentation_refresh_pending = false;
            view.user_timestamps.clear();
            view.live_user_timestamp = None;
            view.live_user_time_accepted = false;
            view.tool_presentations.clear();
            view.completed_steers.clear();
            // A store record is a read fact: the outcome is Confirmed. A
            // failed save stays visible through `last_result.persistence` and
            // `needs_result_confirmation()`.
            view.result_confirmation = ResultConfirmation::Confirmed;
        }
        let opened_id = session_id.clone();
        let mut commands = self.on_session_response(session_id, response);
        if matches!(&self.dock, Dock::SessionSelector(state) if state.selected_session_id.as_deref() == Some(opened_id.as_str()) && matches!(&state.mode, SessionPanelMode::Browse))
        {
            self.dock = Dock::Composer;
        }
        if self.reload.is_some() {
            commands.extend(self.maybe_finish_reload());
        }
        commands
    }

    pub(super) fn on_session_state_response(
        &mut self,
        session_id: &SessionId,
        query: u64,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        let Some(view) = self.sessions.known.get(session_id) else {
            return Vec::new();
        };
        if view.latest_state_query != Some(query) {
            return Vec::new();
        }
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.latest_state_query = None;
        }
        match response.parse_session_state() {
            Ok(state) if state.session_id.as_str() == session_id.as_str() => {
                let commands =
                    self.apply_session_state(&state, None, SessionStateSource::FreshResponse);
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    if view.manual_compact.as_ref().is_some_and(|compact| {
                        compact.result.as_ref().is_some_and(|result| {
                            result.status == crate::protocol::CompactStatusWire::UnknownWrite
                        })
                    }) {
                        view.manual_compact
                            .as_mut()
                            .expect("manual compact was checked")
                            .state_refresh_confirmed = true;
                    }
                }
                self.maybe_clear_unknown_compact_fence(session_id);
                commands
            }
            Ok(_) => {
                self.mark_session_uncalibrated(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("state response does not match requested session {session_id}"),
                );
                Vec::new()
            }
            Err(error) => {
                self.mark_session_uncalibrated(session_id);
                self.notice(
                    NoticeLevel::Warning,
                    format!("failed to fetch state for {session_id}: {error}"),
                );
                Vec::new()
            }
        }
    }

    pub(super) fn on_session_presentation_response(
        &mut self,
        session_id: &SessionId,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.sessions.deleted.contains(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        let parsed = response.parse_session_presentation();
        // Captured before the refresh closure so the receipt can be reconciled
        // after the view borrow is released (dropped-event recovery).
        let receipt = parsed
            .as_ref()
            .ok()
            .and_then(|presentation| {
                (presentation.session_id == *session_id)
                    .then(|| presentation.steer_progress.clone())
            })
            .flatten();
        let refresh = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            view.presentation_pending = false;
            let refresh = view.presentation_refresh_pending;
            view.presentation_refresh_pending = false;
            match parsed {
                Ok(presentation) if presentation.session_id == *session_id => {
                    view.presentation = Some(presentation);
                }
                Ok(_) => self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "session.presentation response does not match requested session {session_id}"
                    ),
                ),
                Err(error) => self.notice(
                    NoticeLevel::Warning,
                    format!("failed to fetch presentation for {session_id}: {error}"),
                ),
            }
            refresh
        };
        if let Some(progress) = receipt {
            self.reconcile_steer_receipt(
                session_id,
                &progress.loop_id,
                progress.request_index,
                progress.applied_count,
            );
        }
        if refresh {
            self.request_session_presentation(session_id)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }
    }

    pub(super) fn on_session_context_response(
        &mut self,
        session_id: &SessionId,
        generation: u64,
        owner: ContextQueryOwner,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let context = match response.parse_session_context() {
            Ok(context) if context.session_id == *session_id => context,
            Ok(_) => {
                self.reschedule_context_poll(session_id, &owner);
                self.notice(
                    NoticeLevel::Warning,
                    format!("session.context response does not match {session_id}"),
                );
                return Vec::new();
            }
            Err(error) => {
                self.reschedule_context_poll(session_id, &owner);
                self.notice(
                    NoticeLevel::Warning,
                    format!("failed to read context for {session_id}: {error}"),
                );
                return Vec::new();
            }
        };

        let (current_operation, cancel_submission, cancel_manual, keep_polling) = {
            let Some(view) = self.sessions.known.get_mut(session_id) else {
                return Vec::new();
            };
            if view.context_query_generation != generation {
                return Vec::new();
            }
            let current_operation = context.current_operation.clone();
            let automatic_active = context.automatic.current.is_some();
            view.context = Some(context);
            if let Some(compact) = view.manual_compact.as_mut() {
                if compact.result.as_ref().is_some_and(|result| {
                    result.status == crate::protocol::CompactStatusWire::UnknownWrite
                }) {
                    compact.context_refresh_confirmed = true;
                }
            }
            if let Some(state) = view.state.as_mut() {
                state.compaction = current_operation.clone();
            }
            let cancel_submission = match &owner {
                ContextQueryOwner::Submission(local_id) => self
                    .submissions
                    .get(local_id)
                    .is_some_and(|submission| submission.cancel_requested),
                _ => false,
            };
            let cancel_manual = match &owner {
                ContextQueryOwner::ManualCompact(operation_id) => {
                    view.manual_compact.as_ref().is_some_and(|compact| {
                        compact.operation_id == *operation_id
                            && compact.result.is_none()
                            && compact.cancel_requested
                    })
                }
                _ => false,
            };
            let keep_polling = current_operation.is_some()
                || automatic_active
                || matches!(owner, ContextQueryOwner::Submission(_))
                    && self
                        .submissions
                        .keys()
                        .any(|local_id| matches!(&owner, ContextQueryOwner::Submission(owner_id) if owner_id == local_id));
            (
                current_operation,
                cancel_submission,
                cancel_manual,
                keep_polling,
            )
        };

        self.maybe_clear_unknown_compact_fence(session_id);

        if keep_polling {
            let due = self
                .instant_now()
                .checked_add(self.context_interval(session_id))
                .expect("context poll deadline is representable");
            self.context_polls.insert(
                session_id.clone(),
                ContextPoll {
                    owner: owner.clone(),
                    due,
                },
            );
        } else if !matches!(owner, ContextQueryOwner::Explicit) {
            self.context_polls.remove(session_id);
        }

        if let Some(operation) = current_operation {
            if let Some(submission) = match &owner {
                ContextQueryOwner::Submission(local_id) => self.submissions.get_mut(local_id),
                _ => None,
            } {
                submission.preparation = Some(OperationRef {
                    session_id: session_id.clone(),
                    operation_id: operation.operation_id.clone(),
                });
            }
            if cancel_submission || cancel_manual {
                return self
                    .request_compact_cancel(session_id, &operation.operation_id)
                    .into_iter()
                    .collect();
            }
        }

        Vec::new()
    }

    pub(super) fn apply_session_state(
        &mut self,
        state: &SessionStateWire,
        event_loop_id: Option<&String>,
        source: SessionStateSource,
    ) -> Vec<AppCommand> {
        let from_event = source == SessionStateSource::Notification;
        if self.sessions.deleted.contains(&state.session_id)
            || self.sessions.pending_deletes.contains(&state.session_id)
        {
            return Vec::new();
        }
        let show_unsupported = {
            let Some(view) = self.sessions.known.get_mut(&state.session_id) else {
                return Vec::new();
            };
            if event_loop_id.is_some_and(|event_loop_id| {
                view.retired_loop
                    .as_ref()
                    .is_some_and(|retired| retired.loop_id == event_loop_id.as_str())
            }) {
                return Vec::new();
            }
            if event_loop_id.is_some_and(|event_loop_id| {
                view.live
                    .as_ref()
                    .is_none_or(|live| live.reference.is_none())
                    && Self::is_prior_loop(view, event_loop_id)
            }) {
                return Vec::new();
            }
            if event_loop_id.is_some_and(|event_loop_id| {
                view.live
                    .as_ref()
                    .and_then(|live| live.reference.as_ref())
                    .is_some_and(|reference| reference.loop_id.as_str() != event_loop_id.as_str())
            }) {
                return Vec::new();
            }
            if from_event
                && event_loop_id.is_none()
                && state.status == SessionStatusWire::Idle
                && view
                    .live
                    .as_ref()
                    .is_some_and(|live| live.reference.is_some())
            {
                return Vec::new();
            }
            if let Some(reference) = view.live.as_ref().and_then(|live| live.reference.as_ref()) {
                if state.active_loop.as_ref().is_some_and(|loop_state| {
                    loop_state.loop_id.as_str() != reference.loop_id.as_str()
                }) || (state.status != SessionStatusWire::Idle
                    && state.status != SessionStatusWire::Blocked
                    && state.active_loop.is_none())
                {
                    return Vec::new();
                }
            }
            if view.live.is_none()
                && state.status != SessionStatusWire::Idle
                && event_loop_id.is_some_and(|event_loop_id| {
                    view.last_result.as_ref().is_some_and(|result| {
                        result.turn.loop_id.as_str() == event_loop_id.as_str()
                    })
                })
            {
                return Vec::new();
            }
            let was_waiting = view
                .state
                .as_ref()
                .is_some_and(|old| old.status == SessionStatusWire::WaitingForInput);
            let mut state = state.clone();
            if view.unsaved_loop.is_some() && state.status != SessionStatusWire::Blocked {
                state.status = SessionStatusWire::Blocked;
                state.block_reason = Some(crate::protocol::SessionBlockReasonWire::Persistence);
            }
            if view.live.is_none() && state.status != SessionStatusWire::Idle {
                if let Some(loop_state) = state.active_loop.as_ref() {
                    let mut live = LiveLoop::new(LocalSubmissionId(u64::MAX), String::new());
                    live.reference = Some(TurnRef {
                        session_id: state.session_id.clone(),
                        loop_id: loop_state.loop_id.clone(),
                    });
                    live.event_gap = true;
                    view.live = Some(live);
                    view.event_gap = true;
                }
            }
            if state.status == SessionStatusWire::Idle
                && view.unsaved_loop.is_none()
                && view
                    .live
                    .as_ref()
                    .is_some_and(|live| live.local_submission == LocalSubmissionId(u64::MAX))
            {
                view.live = None;
            }
            view.state = Some(state.clone());
            if source == SessionStateSource::FreshResponse
                && state.status == SessionStatusWire::Running
                && state.active_loop.as_ref().is_some_and(|loop_state| {
                    view.live
                        .as_ref()
                        .and_then(|live| live.reference.as_ref())
                        .is_some_and(|reference| reference.loop_id == loop_state.loop_id)
                })
            {
                // A reload-staged state, an Idle notification, and a fresh
                // Idle response are not Steer authority. Only a matching
                // normal state response proving the current TurnRef is still
                // Running releases this independent fence.
                view.steer_state_unconfirmed = false;
            }
            if matches!(
                source,
                SessionStateSource::FreshResponse | SessionStateSource::CloseVerifyResponse
            ) {
                view.close_verification_unknown = false;
            }
            !was_waiting && state.status == SessionStatusWire::WaitingForInput
        };
        if show_unsupported {
            self.sticky_notice(NoticeLevel::Warning, UNSUPPORTED_INTERACTION_NOTICE);
        }
        Vec::new()
    }

    pub(super) fn on_update_session_response(
        &mut self,
        session_id: SessionId,
        target_loop_id: Option<String>,
        model: Option<String>,
        reasoning: Option<Reasoning>,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let mut refresh_presentation = false;
        match response.parse_session_update() {
            Ok(result) => {
                let active_revision = result.active_revision;
                let session = result.session;
                if session.session_id != session_id {
                    self.notice(
                        NoticeLevel::Warning,
                        format!(
                            "session.update response does not match requested session {session_id}"
                        ),
                    );
                    return Vec::new();
                }
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    // Session.update successful SessionInfo is always the durable authority for the session
                    // (at most one update in-flight per session) and must not be discarded because a loop finished.
                    view.info = session.clone();

                    let current_live_loop = view
                        .live
                        .as_ref()
                        .and_then(|l| l.reference.as_ref().map(|r| &r.loop_id));
                    let is_different_new_loop = match (&target_loop_id, current_live_loop) {
                        (Some(t_loop), Some(c_loop)) => t_loop != c_loop,
                        _ => false,
                    };

                    // Only actual-request applied evidence requires the same loop.
                    // When the loop has already finished, it becomes SavedNextTurn or reflects already observed evidence.
                    // Old responses across loops must not retag new requests.
                    if !is_different_new_loop {
                        let applied = active_revision.is_some_and(|revision| {
                            view.last_request.as_ref().is_some_and(|request| {
                                request.revision == revision
                                    && target_loop_id
                                        .as_ref()
                                        .is_none_or(|tl| request.loop_id.as_ref() == Some(tl))
                                    && request.model == session.model
                                    && request.reasoning == session.reasoning
                            })
                        });

                        view.config_update = Some(crate::state::session::PendingConfigUpdate {
                            loop_id: target_loop_id,
                            model,
                            reasoning,
                            revision: active_revision,
                            state: if applied {
                                crate::state::session::ConfigUpdateState::Applied
                            } else if view.live.is_none() {
                                crate::state::session::ConfigUpdateState::SavedNextTurn
                            } else if active_revision.is_some() {
                                crate::state::session::ConfigUpdateState::WaitingBoundary
                            } else {
                                crate::state::session::ConfigUpdateState::SavedNextTurn
                            },
                        });
                    }
                }
                self.upsert_session_list(session);
                if self.sessions.active.as_ref() == Some(&session_id)
                    && matches!(
                        &self.dock,
                        Dock::ModelSelector(_) | Dock::ReasoningSelector(_)
                    )
                {
                    self.dock = Dock::Composer;
                }
                if let Some(revision) = active_revision {
                    self.notice(
                        NoticeLevel::Info,
                        format!("Saved · applies at next model request (rev {revision})"),
                    );
                } else if self.sessions.known.get(&session_id).is_some_and(|view| {
                    view.state
                        .as_ref()
                        .is_some_and(|state| state.status != SessionStatusWire::Idle)
                }) {
                    self.notice(
                        NoticeLevel::Info,
                        "Saved for next turn; no active revision was returned.",
                    );
                } else {
                    self.notice(NoticeLevel::Info, "Updated for next turn");
                }
                refresh_presentation = true;
            }
            Err(error) => {
                let message = error.to_string();
                if let Some(view) = self.sessions.known.get_mut(&session_id) {
                    let current_loop_id = view
                        .live
                        .as_ref()
                        .and_then(|l| l.reference.as_ref().map(|r| r.loop_id.clone()));
                    view.config_update = Some(crate::state::session::PendingConfigUpdate {
                        loop_id: current_loop_id,
                        model: model.clone(),
                        reasoning,
                        revision: None,
                        state: crate::state::session::ConfigUpdateState::Failed(message.clone()),
                    });
                }
                let matches_active = self.sessions.active.as_ref() == Some(&session_id);
                let selector_state = if matches_active {
                    match &mut self.dock {
                        Dock::ModelSelector(state) | Dock::ReasoningSelector(state) => Some(state),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(state) = selector_state {
                    state.submitting = false;
                    state.error = Some(message);
                } else {
                    self.notice(
                        NoticeLevel::Warning,
                        format!("failed to update session {session_id}: {error}"),
                    );
                }
            }
        }
        if refresh_presentation {
            if let Some(command) = self.request_session_presentation(&session_id) {
                return vec![command];
            }
        }
        Vec::new()
    }
}
