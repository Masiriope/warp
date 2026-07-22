use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::FairMutex;
#[cfg(feature = "voice_input")]
use warpui::SingletonEntity;
use warpui::{AppContext, Entity, EntityId, ModelContext, ModelHandle};

use super::core::subscribe_to_shared_dependencies;
use super::{
    InlineItem, SlashCommandDataSource, SlashCommandDataSourceState, UpdatedActiveCommands,
};
#[cfg(feature = "voice_input")]
use crate::ai::AIRequestUsageModel;
use crate::ai::blocklist::block::cli_controller::CLISubagentController;
use crate::search::SyncDataSource;
use crate::search::data_source::{Query, QueryResult};
use crate::search::mixer::DataSourceRunErrorWrapper;
use crate::search::slash_command_menu::static_commands::Availability;
use crate::search::slash_command_menu::static_commands::commands::COMMAND_REGISTRY;
#[cfg(feature = "voice_input")]
use crate::settings::AISettings;
use crate::terminal::TerminalModel;
use crate::terminal::input::slash_commands::{
    AcceptSlashCommandOrSavedPrompt, slash_command_is_supported_in_tui,
};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::view::resolve_ai_query_routing;
#[cfg(feature = "voice_input")]
use crate::workspaces::user_workspaces::UserWorkspaces;
#[cfg(any(feature = "voice_input", test))]
fn voice_command_gates_pass(
    ai_enabled: bool,
    team_enabled: bool,
    quota_available: bool,
    local_routing: bool,
) -> bool {
    ai_enabled && team_enabled && quota_available && local_routing
}

pub struct TuiDataSourceArgs {
    pub active_session: ModelHandle<ActiveSession>,
    pub cli_subagent_controller: ModelHandle<CLISubagentController>,
    pub terminal_view_id: EntityId,
    pub terminal_model: Arc<FairMutex<TerminalModel>>,
}

pub struct TuiSlashCommandDataSource {
    state: SlashCommandDataSourceState,
    terminal_model: Arc<FairMutex<TerminalModel>>,
}

impl TuiSlashCommandDataSource {
    pub fn new(args: TuiDataSourceArgs, ctx: &mut ModelContext<Self>) -> Self {
        let TuiDataSourceArgs {
            active_session,
            cli_subagent_controller,
            terminal_view_id,
            terminal_model,
        } = args;

        subscribe_to_shared_dependencies(
            &active_session,
            &cli_subagent_controller,
            terminal_view_id,
            Self::recompute_active_commands,
            ctx,
        );
        #[cfg(feature = "voice_input")]
        {
            ctx.subscribe_to_model(
                &crate::settings::AISettings::handle(ctx),
                |me, _, event, ctx| {
                    if matches!(
                        event,
                        crate::settings::AISettingsChangedEvent::VoiceInputEnabled { .. }
                    ) {
                        me.recompute_active_commands(ctx);
                    }
                },
            );
            ctx.subscribe_to_model(&UserWorkspaces::handle(ctx), |me, _, event, ctx| {
                if matches!(
                    event,
                    crate::workspaces::user_workspaces::UserWorkspacesEvent::TeamsChanged
                ) {
                    me.recompute_active_commands(ctx);
                }
            });
            ctx.subscribe_to_model(&AIRequestUsageModel::handle(ctx), |me, _, event, ctx| {
                if matches!(
                    event,
                    crate::ai::AIRequestUsageModelEvent::RequestUsageUpdated
                ) {
                    me.recompute_active_commands(ctx);
                }
            });
        }

        let mut me = Self {
            state: SlashCommandDataSourceState::new(
                active_session,
                cli_subagent_controller,
                terminal_view_id,
            ),
            terminal_model,
        };
        me.recompute_active_commands(ctx);
        me
    }

    /// Returns whether this TUI surface routes AI work to its local execution host.
    ///
    /// This reuses the GUI's canonical routing decision. TUI surfaces have no
    /// `AmbientAgentViewModel`, so shared-session state comes from the terminal model.
    pub fn local_skills_available(&self, app: &AppContext) -> bool {
        let terminal_model = self.terminal_model.lock();
        resolve_ai_query_routing(self.terminal_view_id(), None, &terminal_model, app).is_local()
    }
    pub fn set_active_repo_root(
        &mut self,
        repo_root: Option<PathBuf>,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.update_active_repo_root(repo_root) {
            self.recompute_active_commands(ctx);
        }
    }

    fn recompute_active_commands(&mut self, ctx: &mut ModelContext<Self>) {
        let availability = self.availability(ctx);
        let gates = self.common_command_gates(ctx);
        let commands = HashMap::from_iter(
            COMMAND_REGISTRY
                .all_commands_by_id()
                .filter(|(_, command)| {
                    slash_command_is_supported_in_tui(command)
                        && self.command_passes_voice_gates(command, ctx)
                        && self.command_passes_common_gates(command, availability, &gates)
                })
                .map(|(id, command)| (id, command.clone())),
        );
        if self.replace_active_commands(commands) {
            ctx.emit(UpdatedActiveCommands);
        }
    }

    fn availability(&self, ctx: &AppContext) -> Availability {
        self.base_availability(ctx)
            | Availability::AGENT_VIEW
            | Availability::ACTIVE_CONVERSATION
            | Availability::NOT_CLOUD_AGENT
    }

    #[cfg(feature = "voice_input")]
    fn command_passes_voice_gates(
        &self,
        command: &crate::search::slash_command_menu::StaticCommand,
        ctx: &AppContext,
    ) -> bool {
        if command.name != crate::search::slash_command_menu::static_commands::commands::VOICE.name
        {
            return true;
        }

        voice_command_gates_pass(
            AISettings::as_ref(ctx).is_voice_input_enabled(ctx),
            UserWorkspaces::as_ref(ctx).is_voice_enabled(),
            AIRequestUsageModel::as_ref(ctx).can_request_voice(),
            self.local_skills_available(ctx),
        )
    }

    #[cfg(not(feature = "voice_input"))]
    fn command_passes_voice_gates(
        &self,
        _command: &crate::search::slash_command_menu::StaticCommand,
        _ctx: &AppContext,
    ) -> bool {
        false
    }
}

impl SyncDataSource for TuiSlashCommandDataSource {
    type Action = AcceptSlashCommandOrSavedPrompt;

    fn run_query(
        &self,
        query: &Query,
        app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        if query.text.is_empty() {
            return Ok(vec![]);
        }

        let query_text = query.text.trim().to_lowercase();
        let mut results = self.match_active_commands(&query_text, app);
        if self.local_skills_available(app) {
            results.extend(self.match_skills(&query_text, app));
        }
        Ok(results
            .into_iter()
            .map(|item: InlineItem| item.into())
            .collect())
    }
}

impl SlashCommandDataSource for TuiSlashCommandDataSource {
    fn state(&self) -> &SlashCommandDataSourceState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut SlashCommandDataSourceState {
        &mut self.state
    }
}

impl Entity for TuiSlashCommandDataSource {
    type Event = UpdatedActiveCommands;
}

#[cfg(test)]
mod tests {
    use super::voice_command_gates_pass;

    #[test]
    fn voice_availability_requires_every_gate() {
        assert!(voice_command_gates_pass(true, true, true, true));
        for disabled_gate in 0..4 {
            let mut gates = [true; 4];
            gates[disabled_gate] = false;
            assert!(
                !voice_command_gates_pass(gates[0], gates[1], gates[2], gates[3]),
                "gate {disabled_gate} must hide /voice"
            );
        }
    }
}
