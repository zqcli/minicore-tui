//! Install results only into their captured resident Session.

use super::{App, Overlay, Session, SessionOp, tasks};
use minicore_host_protocol::*;

impl App {
    pub(super) fn adopt(&mut self, opened: tasks::Opened) {
        self.install_opened(opened, true);
    }
    pub(super) fn install_opened(&mut self, mut opened: tasks::Opened, select: bool) -> u64 {
        let target = SessionRef {
            agent_id: opened.info.agent_id.clone(),
            session_id: opened.info.id.clone(),
        };
        if let Some(epoch) = self.resident.find(&target).map(|s| s.epoch) {
            if select {
                self.switch_to(epoch);
            }
            return epoch;
        }
        self.epoch += 1;
        let epoch = self.epoch;
        let mut session = Session::new(
            opened.info,
            opened.state,
            epoch,
            self.client.session(target.clone()),
        );
        session.history_upper_bound = opened.history.upper_bound;
        session.queue = Some(opened.inputs);
        session.context = Some(opened.context);
        let records = std::mem::take(&mut opened.history.items);
        session.history = Some(opened.history);
        session.sync_interaction();
        if let Some(newer) = self.pending_states.remove(&target) {
            session.apply_state(newer);
        }
        self.resident.insert(session);
        self.install_records(epoch, &records);
        if select {
            self.select_session(Some(epoch));
        }
        self.pump_subscription(epoch, opened.subscription);
        self.refresh_session_details_for(epoch);
        self.refresh_workspace_for(epoch);
        epoch
    }
    pub(super) fn on_op(&mut self, epoch: u64, op: SessionOp) {
        match op {
            SessionOp::Observed { generation, event } => {
                if self.session_mut(epoch).subscription_generation == generation {
                    self.on_session_event(epoch, *event);
                }
            }
            SessionOp::Baseline {
                generation,
                baseline,
            } => {
                if self.session_mut(epoch).subscription_generation == generation {
                    self.install_baseline(epoch, baseline);
                }
            }
            SessionOp::Subscribe(result) => {
                self.session_mut(epoch).subscription_loading = false;
                match result {
                    Ok(subscription) => {
                        self.session_mut(epoch).observation_error = None;
                        self.pump_subscription(epoch, subscription);
                        self.refresh_history(epoch);
                    }
                    Err(error) => {
                        self.session_mut(epoch).observation_error = Some(error.clone());
                        self.error(error);
                    }
                }
            }
            SessionOp::ObservationEnded {
                generation,
                error,
                resync,
            } => {
                if self.session_mut(epoch).subscription_generation != generation {
                    return;
                }
                self.session_mut(epoch).observation_error = error.clone().or_else(|| {
                    Some("Observation ended; open /session and press r to refresh".into())
                });
                if resync {
                    self.resubscribe(epoch);
                } else if let Some(error) = error {
                    self.error(error);
                }
            }
            SessionOp::State(result) => match result {
                Ok(state) => self.apply_snapshot(epoch, state),
                Err(error) => self.error(error),
            },
            SessionOp::Info(result) => match result {
                Ok(info) => {
                    let session = self.session_mut(epoch);
                    if info.id == session.info.id
                        && info.agent_id == session.info.agent_id
                        && info.config_revision >= session.info.config_revision
                    {
                        let workspace_changed =
                            session.info.config.workspace != info.config.workspace;
                        session.info = info;
                        if workspace_changed {
                            session.branch = None;
                        }
                        if workspace_changed
                            || (session.workspace_due && !session.workspace_loading)
                        {
                            session.workspace_due = false;
                            self.refresh_workspace_for(epoch);
                        }
                    }
                }
                Err(error) => self.error(error),
            },
            SessionOp::Inputs(result) => {
                let session = self.session_mut(epoch);
                session.inputs_loading = false;
                match result {
                    Ok(inputs) => {
                        if inputs.host_id == session.state.host_id
                            && inputs.host_incarnation == session.state.host_incarnation
                            && inputs.agent_id == session.info.agent_id
                            && inputs.session_id == session.info.id
                            && session.queue.as_ref().is_none_or(|old| {
                                inputs.controller_revision >= old.controller_revision
                            })
                        {
                            session.queue = Some(inputs);
                        }
                    }
                    Err(error) => self.error(error),
                }
                if std::mem::take(&mut self.session_mut(epoch).inputs_due) {
                    self.refresh_inputs(epoch);
                }
            }
            SessionOp::Context(result) => {
                self.session_mut(epoch).context_loading = false;
                match result {
                    Ok(context) => {
                        let session = self.session_mut(epoch);
                        if context.source.host_id == session.state.host_id
                            && context.source.host_incarnation == session.state.host_incarnation
                            && context.source.agent_id == session.info.agent_id
                            && context.source.session_id == session.info.id
                            && context.data.load_id == session.state.data.load_id
                            && session.context.as_ref().is_none_or(|old| {
                                context.data.controller_revision >= old.data.controller_revision
                            })
                        {
                            session.context = Some(context);
                        }
                    }
                    Err(error) => self.error(error),
                }
                if std::mem::take(&mut self.session_mut(epoch).context_due) {
                    self.refresh_context_for(epoch);
                }
            }
            SessionOp::Workspace { revision, result } => {
                self.session_mut(epoch).workspace_loading = false;
                let mut refresh_info = false;
                match result {
                    Ok(workspace) => {
                        let session = self.session_mut(epoch);
                        if session.identity_matches(&workspace.source)
                            && session.info.config.workspace.is_some()
                        {
                            if workspace.config_revision == Some(session.info.config_revision) {
                                session.branch = workspace.data.branch;
                            } else {
                                session.workspace_due = true;
                                refresh_info = true;
                            }
                        }
                    }
                    Err(error) if revision == self.session_mut(epoch).info.config_revision => {
                        self.error(error);
                    }
                    Err(_) => {}
                }
                if refresh_info {
                    self.refresh_session_info_for(epoch);
                } else if std::mem::take(&mut self.session_mut(epoch).workspace_due) {
                    self.refresh_workspace_for(epoch);
                }
            }
            SessionOp::History(result) => {
                self.session_mut(epoch).history_refreshing = false;
                match result {
                    Ok(mut window) => {
                        if !self.session_mut(epoch).identity_matches(&window.source) {
                            return;
                        }
                        let records = std::mem::take(&mut window.items);
                        self.install_records(epoch, &records);
                        let session = self.session_mut(epoch);
                        session.history_upper_bound =
                            session.history_upper_bound.max(window.upper_bound);
                        if session.history.is_none() && !session.loading_older {
                            session.history = Some(window);
                        }
                    }
                    Err(error) => {
                        self.session_mut(epoch).observation_error = Some(error.clone());
                        self.error(error);
                    }
                }
                if std::mem::take(&mut self.session_mut(epoch).history_due) {
                    self.refresh_history(epoch);
                }
            }
            SessionOp::Older { window, result } => {
                if !self.session_mut(epoch).identity_matches(&window.source) {
                    return;
                }
                self.session_mut(epoch).loading_older = false;
                self.session_mut(epoch).history = Some(window);
                match result {
                    Ok(records) => self.install_records(epoch, &records),
                    Err(error) => {
                        self.agent_location = None;
                        self.error(error);
                    }
                }
            }
            SessionOp::Turn(turn, result) => {
                self.session_mut(epoch).turn_reads.remove(&turn);
                match result {
                    Ok(result) => {
                        if result.source.turn_id == turn
                            && self.session_mut(epoch).turn_matches(&result.source)
                        {
                            let session = self.session_mut(epoch);
                            if let Some(error) = session.failed_turn_reads.remove(&turn)
                                && session.observation_error.as_ref() == Some(&error)
                            {
                                session.observation_error = session
                                    .failed_turn_reads
                                    .values()
                                    .chain(session.failed_tool_reads.values())
                                    .next()
                                    .cloned();
                            }
                            self.install_turn(epoch, result);
                        }
                    }
                    Err(error) => {
                        let error = format!("Turn {turn} result: {error}");
                        self.session_mut(epoch)
                            .failed_turn_reads
                            .insert(turn, error.clone());
                        self.session_mut(epoch).observation_error = Some(error.clone());
                        self.error(error);
                    }
                }
            }
            SessionOp::Tool(tool, result) => {
                self.session_mut(epoch).tool_reads.remove(&tool);
                match result {
                    Ok(read) => {
                        if read.source.turn.turn_id == tool.turn_id
                            && read.source.tool_call_id == tool.tool_call_id
                            && self.session_mut(epoch).turn_matches(&read.source.turn)
                        {
                            let session = self.session_mut(epoch);
                            if let Some(error) = session.failed_tool_reads.remove(&tool)
                                && session.observation_error.as_ref() == Some(&error)
                            {
                                session.observation_error = session
                                    .failed_turn_reads
                                    .values()
                                    .chain(session.failed_tool_reads.values())
                                    .next()
                                    .cloned();
                            }
                            self.install_tool(epoch, read);
                        }
                    }
                    Err(error) => {
                        let error = format!(
                            "Tool {} in Turn {}: {error}",
                            tool.tool_call_id, tool.turn_id
                        );
                        self.session_mut(epoch)
                            .failed_tool_reads
                            .insert(tool, error.clone());
                        self.session_mut(epoch).observation_error = Some(error.clone());
                        self.error(error);
                    }
                }
            }
            SessionOp::Output {
                source,
                channel,
                result,
            } => self.output_read_result(epoch, source, channel, result),
            SessionOp::Mutation { label, result } => {
                if let Err(error) = result {
                    self.error(format!("{label}: {error}"));
                }
                self.refresh_control_for(epoch);
                self.refresh_inputs(epoch);
                self.refresh_session_details_for(epoch);
                self.refresh_catalogs();
            }
            SessionOp::Cancelled { turn_id, result } => {
                if let Err(error) = result {
                    if let Some(live) = &mut self.session_mut(epoch).live
                        && live.turn_id == turn_id
                    {
                        live.interrupting = false;
                    }
                    self.error(format!("Cancel Turn {turn_id}: {error}"));
                }
                self.refresh_control_for(epoch);
                self.refresh_inputs(epoch);
            }
            SessionOp::Answered {
                turn_id,
                load_id,
                interaction_id,
                result,
            } => {
                if let Err(error) = result {
                    if let Some(pending) = &mut self.session_mut(epoch).interaction
                        && pending.turn_id == turn_id
                        && pending.load_id == load_id
                        && pending.data.interaction_id == interaction_id
                    {
                        pending.submitted = false;
                    }
                    self.error(format!("Answer interaction {interaction_id}: {error}"));
                }
                self.refresh_control_for(epoch);
            }
            SessionOp::Recalled { removed, error } => {
                for record in removed {
                    if self.resident.is_current(epoch) {
                        self.restore_to_composer(&record.input.text);
                    } else {
                        self.session_mut(epoch).returned.push(record.input.text);
                    }
                }
                if let Some(error) = error {
                    self.error(error);
                }
                self.refresh_inputs(epoch);
                self.refresh_control_for(epoch);
            }
            SessionOp::Files(result) => {
                if !self.resident.is_current(epoch) {
                    return;
                }
                match result {
                    Ok(paths) => {
                        if let Overlay::Files { paths: current, .. } = &mut self.overlay {
                            *current = paths;
                        }
                    }
                    Err(error) => self.error(error),
                }
            }
            SessionOp::ChangeFiles(result) => {
                if self.resident.is_current(epoch) {
                    self.on_change_files(result);
                }
            }
            SessionOp::DiffFile(query, result) => {
                if self.resident.is_current(epoch) {
                    self.on_diff_file(query, result);
                }
            }
        }
    }
    pub(super) fn list_files(&self) {
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        self.spawn_for(epoch, async move {
            let result = async {
                let mut cursor = None;
                let mut paths = Vec::new();
                loop {
                    let page = client
                        .workspace_files(
                            minicore_host_workspace::FilesRequest {
                                directory: String::new(),
                                recursive: true,
                                query: None,
                                cursor,
                                limit: 500,
                            },
                            tasks::options(),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    paths.extend(
                        page.data
                            .entries
                            .into_iter()
                            .filter(|entry| {
                                entry.kind != minicore_host_workspace::FileKind::Directory
                            })
                            .map(|entry| entry.path),
                    );
                    cursor = page.data.next_cursor;
                    if cursor.is_none() {
                        break;
                    }
                }
                Ok(paths)
            }
            .await;
            SessionOp::Files(result)
        });
    }
}
