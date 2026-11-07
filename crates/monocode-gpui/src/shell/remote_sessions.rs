//! Project indexes are fetched on workers; opening a row materializes the chat.
use super::*;
use serde_json::json;

impl Shell {
    pub(super) fn refresh_project_links(&mut self, cx: &mut Context<Shell>) -> bool {
        let local_projects = self.workspace.projects.clone();
        let links = locked(&self.store)
            .and_then(|store| store.project_mappings(&local_projects))
            .unwrap_or_default();
        let links_changed = links != self.project_links;
        self.project_links = links;
        let previous_machines = self.remote_machines.clone();
        if let Ok(machines) = self.remote.list() {
            self.remote_machines = machines;
        }
        let machines_changed = previous_machines != self.remote_machines;
        if links_changed || machines_changed {
            let previous_hosts = self.workspace.project_hosts.clone();
            let machines = &self.remote_machines;
            let links = &self.project_links;
            self.workspace.project_hosts.retain(|project, environment| {
                let environment = environment.as_str();
                machines
                    .iter()
                    .any(|machine| machine.environment_id.as_str() == environment)
                    && links
                        .get(project)
                        .is_some_and(|mapped| mapped.contains_key(environment))
            });
            let views: Vec<_> = self
                .sessions
                .iter()
                .map(|(id, view)| (*id, view.read(cx).summary.cwd.clone()))
                .collect();
            for (id, cwd) in views {
                if let Some(view) = self.sessions.get(&id) {
                    self.configure_project_hosts(view, &cwd, cx);
                }
            }
            if self.workspace.project_hosts != previous_hosts {
                self.persist(cx);
            }
        }
        links_changed || machines_changed
    }

    pub(super) fn canonical_project_path(&self, path: &str) -> String {
        monocode_core::store::local_project_for_remote(&self.project_links, path)
            .unwrap_or(path)
            .to_owned()
    }

    pub(super) fn project_matches_session(&self, project: &str, cwd: &str) -> bool {
        cwd == project
            || self
                .project_links
                .get(project)
                .is_some_and(|mappings| mappings.values().any(|remote| remote.key == cwd))
    }

    pub(super) fn visible_project_paths(&self) -> Vec<String> {
        self.workspace
            .projects
            .iter()
            .filter(|path| {
                monocode_core::store::local_project_for_remote(&self.project_links, path).is_none()
            })
            .cloned()
            .collect()
    }

    pub(super) fn configure_project_hosts(
        &self,
        view: &Entity<Conversation>,
        cwd: &str,
        cx: &mut Context<Shell>,
    ) {
        let project = self.canonical_project_path(cwd);
        let mappings = self.project_links.get(&project);
        let available = mappings.is_some_and(|entries| !entries.is_empty());
        let choices = self
            .remote_machines
            .iter()
            .map(|machine| HostChoice {
                environment_id: machine.environment_id.clone(),
                name: machine.name.clone(),
                mapped: mappings
                    .is_some_and(|entries| entries.contains_key(&machine.environment_id)),
            })
            .collect();
        let selected = self
            .workspace
            .project_hosts
            .get(&project)
            .filter(|environment| {
                mappings.is_some_and(|entries| entries.contains_key(*environment))
            })
            .cloned();
        view.update(cx, |view, cx| {
            view.configure_project_hosts(project, available, choices, selected, cx)
        });
    }

    /// The cwd a new session in `project` starts with, and its chosen machine.
    /// A chosen machine that cannot take the session is an error: a new session
    /// never falls back to running on this Mac.
    pub(super) fn new_session_cwd(
        &self,
        project: &str,
    ) -> Result<(String, Option<String>), String> {
        let Some(environment) = self.workspace.project_hosts.get(project) else {
            return Ok((project.to_owned(), None));
        };
        let machine = self
            .remote_machines
            .iter()
            .find(|machine| machine.environment_id == *environment);
        machine
            .and_then(|_| {
                monocode_core::store::project_cwd_for_host(
                    project,
                    &self.project_links,
                    Some(environment),
                )
            })
            .map(|cwd| (cwd, Some(environment.clone())))
            .ok_or_else(|| {
                format!(
                    "{} cannot start this session: the folder is not open on it. Check Settings → Connections, or choose This Mac.",
                    machine.map_or("The chosen machine", |machine| machine.name.as_str())
                )
            })
    }

    /// A session cannot change machine once it exists, so an unstarted one
    /// follows the Host chip: a new session on the chosen machine replaces it
    /// and keeps its draft and
    /// its picked model controls.
    pub(super) fn follow_host_choice(&mut self, id: i64, project: &str, cx: &mut Context<Self>) {
        if self.workspace.project != project {
            return;
        }
        let Some((cwd, kind, composer)) = self.sessions.get(&id).and_then(|view| {
            let view = view.read(cx);
            Some((
                view.summary.cwd.clone(),
                Kind::from_id(&view.summary.provider),
                view.unstarted_composer(cx)?,
            ))
        }) else {
            return;
        };
        match self.new_session_cwd(project) {
            Ok((target, _)) if target != cwd => self.create_session_target(
                self.explicit_choice(kind),
                false,
                None,
                Some((id, composer.0, composer.1)),
                cx,
            ),
            Ok(_) => {}
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    pub(super) fn session_machine_tag(&self, cwd: &str) -> Option<String> {
        if let Some(path) = monocode_core::remote::parse_remote_path(cwd) {
            let linked =
                monocode_core::store::local_project_for_remote(&self.project_links, cwd).is_some();
            if !linked {
                return None;
            }
            Some(
                self.remote_machines
                    .iter()
                    .find(|machine| machine.environment_id == path.environment_id)
                    .map(|machine| machine.name.clone())
                    .unwrap_or_else(|| "Disconnected".into()),
            )
        } else {
            self.project_links
                .get(cwd)
                .is_some_and(|mappings| !mappings.is_empty())
                .then(|| "This Mac".into())
        }
    }

    pub(super) fn refresh_remote_projects(&mut self, cx: &mut Context<Self>) {
        let links_changed = self.refresh_project_links(cx);
        if links_changed {
            self.invalidate_chrome(cx);
        }
        if self.remote_refreshing {
            return;
        }
        let context_project = self.workspace.project.clone();
        let store = self.store.clone();
        let projects = if context_project.starts_with("remote://") {
            locked(&store)
                .and_then(|store| store.remote_projects())
                .unwrap_or_default()
                .into_values()
                .filter(|project| project.key == context_project)
                .collect::<Vec<_>>()
        } else {
            self.project_links
                .get(&context_project)
                .map(|projects| projects.values().cloned().collect())
                .unwrap_or_default()
        };
        if projects.is_empty() {
            return;
        }
        self.remote_refreshing = true;
        let client = self.remote.clone();
        let work = cx.background_executor().spawn(async move {
            let mut updates = Vec::new();
            let mut errors = Vec::new();
            for project in projects {
                let result = (|| -> Result<_, String> {
                    let machine = client.machine_for_environment(&project.environment_id)?;
                    let response = client.request(
                        &machine.id,
                        "sessions.list",
                        json!({"projectId":project.project_id}),
                    )?;
                    let values = response.as_array().ok_or("Invalid remote session list")?;
                    let store = locked(&store)?;
                    let host_ids: HashSet<String> = values
                        .iter()
                        .filter_map(|value| value["id"].as_str().map(str::to_owned))
                        .collect();
                    store.prune_remote_sessions(&project, &host_ids)?;
                    let mut summaries = Vec::new();
                    for value in values {
                        let summary = store.register_remote_summary(&project, value)?;
                        let state = store
                            .remote_session(summary.id)?
                            .ok_or("Remote session identity is missing")?;
                        summaries.push((
                            summary,
                            state.running,
                            state.needs_input,
                            value["draft"] == true,
                        ));
                    }
                    let retained: HashSet<i64> = store
                        .sessions()?
                        .iter()
                        .filter(|session| session.cwd == project.key)
                        .map(|session| session.id)
                        .collect();
                    Ok((project.key.clone(), summaries, retained))
                })();
                match result {
                    Ok(update) => updates.push(update),
                    Err(error) => errors.push(error),
                }
            }
            if updates.is_empty() && !errors.is_empty() {
                return Err(errors.join("; "));
            }
            Ok::<_, String>((context_project, updates, errors))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.remote_refreshing = false;
                // The poll repeats every few seconds: repaint only for a change.
                let mut changed = false;
                match result {
                    Ok((context, updates, errors)) => {
                        for (project, summaries, retained) in updates {
                            let removed: Vec<_> = this
                                .order
                                .iter()
                                .copied()
                                .filter(|id| {
                                    this.session_summaries
                                        .get(id)
                                        .is_some_and(|s| s.cwd == project && !retained.contains(id))
                                })
                                .collect();
                            changed |= !removed.is_empty();
                            for id in removed {
                                this.remote_marks.remove(&id);
                                this.order.retain(|existing| *existing != id);
                                if let Some(view) = this.sessions.get(&id) {
                                    view.update(cx, |view, cx| {
                                        view.remote_connection_error(
                                            "This conversation was deleted on the host.",
                                            cx,
                                        )
                                    });
                                }
                                // Open tabs retain an explanatory placeholder. A stale
                                // tab must not be rebound to a new host conversation.
                                if this.workspace.tab_of(id).is_none() {
                                    this.session_summaries.remove(&id);
                                    this.sessions.remove(&id);
                                }
                            }
                            for (summary, running, waiting, draft) in summaries {
                                let id = summary.id;
                                changed |= this.remote_marks.insert(id, (running, waiting))
                                    != Some((running, waiting));
                                changed |= if draft {
                                    this.draft_sessions.insert(id)
                                } else {
                                    this.draft_sessions.remove(&id)
                                };
                                if !this.session_summaries.contains_key(&id) {
                                    this.order.push(id);
                                }
                                if let Some(view) = this.sessions.get(&id) {
                                    view.update(cx, |view, cx| {
                                        if !view.busy && view.summary != summary {
                                            view.summary = summary.clone();
                                            cx.notify();
                                        } else if view.busy && view.summary.title != summary.title {
                                            // A running turn owns the rest; a host title is metadata.
                                            view.summary.title = summary.title.clone();
                                            cx.emit(crate::conversation::SessionChanged);
                                            cx.notify();
                                        }
                                    });
                                }
                                changed |= this.session_summaries.get(&id) != Some(&summary);
                                this.session_summaries.insert(id, summary);
                            }
                        }
                        if !errors.is_empty()
                            && let Some(id) = this.workspace.active
                            && let Some(view) =
                                this.sessions.get(&this.workspace.session_for_pane(id))
                        {
                            view.update(cx, |view, cx| {
                                view.remote_connection_error(&errors.join("; "), cx)
                            });
                        }
                        if changed && this.workspace.project == context {
                            this.refresh_sidebar_structure(cx);
                            this.invalidate_chrome(cx);
                        }
                    }
                    Err(error) => {
                        // A transient disconnect retains the last host index.
                        if let Some(id) = this.workspace.active
                            && let Some(view) =
                                this.sessions.get(&this.workspace.session_for_pane(id))
                        {
                            view.update(cx, |view, cx| view.remote_connection_error(&error, cx));
                        }
                    }
                }
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
#[path = "remote_title_tests.rs"]
mod remote_title_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ProjectHostSelected;
    use gpui::TestAppContext;

    const PROJECT: &str = "/work/monocode";

    fn fedora() -> monocode_core::remote::Machine {
        monocode_core::remote::Machine {
            id: "machine".into(),
            name: "Fedora".into(),
            endpoint: "ssh://fedora".into(),
            environment_id: "fedora".into(),
            ssh: None,
        }
    }

    /// Map PROJECT to a Fedora folder and make Fedora the connected machine.
    fn connect_fedora(shell: &mut Shell) -> String {
        let remote = locked(&shell.store)
            .unwrap()
            .remember_remote_project("fedora", &json!({"id":"project","cwd":"/home/me/monocode"}))
            .unwrap();
        let key = remote.key.clone();
        shell.remote_machines = vec![fedora()];
        shell.project_links = [(PROJECT.to_owned(), [("fedora".to_owned(), remote)].into())].into();
        key
    }

    /// Step the executor until `done`. `run_until_parked` cannot be used once a
    /// remote chat is open: with no host connection in a test its reload fails
    /// at once and retries on every redraw, so the executor never parks.
    fn settle(
        cx: &mut TestAppContext,
        window: gpui::WindowHandle<Shell>,
        done: impl Fn(&Shell, &App) -> bool,
    ) {
        for _ in 0..20_000 {
            cx.executor().tick();
            if window.update(cx, |shell, _, cx| done(shell, cx)).unwrap() {
                return;
            }
        }
        panic!("the session did not move");
    }

    #[gpui::test]
    fn a_chosen_machine_is_never_replaced_by_this_mac(cx: &mut TestAppContext) {
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, _, _| {
                assert_eq!(
                    shell.new_session_cwd(PROJECT),
                    Ok((PROJECT.to_owned(), None))
                );
                shell
                    .workspace
                    .project_hosts
                    .insert(PROJECT.into(), "fedora".into());
                let error = shell.new_session_cwd(PROJECT).unwrap_err();
                assert!(error.contains("cannot start this session"), "{error}");
                let key = connect_fedora(shell);
                assert_eq!(
                    shell.new_session_cwd(PROJECT),
                    Ok((key, Some("fedora".to_owned())))
                );
                shell.project_links.clear();
                assert!(
                    shell
                        .new_session_cwd(PROJECT)
                        .unwrap_err()
                        .contains("Fedora")
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn choosing_a_host_moves_the_unstarted_session_and_keeps_its_draft(cx: &mut TestAppContext) {
        let (window, _, blank) = super::super::tests::window(cx);
        let key = window
            .update(cx, |shell, _, cx| {
                let key = connect_fedora(shell);
                shell.sessions[&blank].update(cx, |view, cx| {
                    view.input.update(cx, |input, cx| {
                        input.set_text("Run this on Fedora".into(), cx)
                    });
                    assert_eq!(view.summary.cwd, PROJECT);
                    view.settings.model = Some("picked-model".into());
                    view.settings.effort = Some("high".into());
                    // The chip's choice is what the Host menu emits.
                    cx.emit(ProjectHostSelected(PROJECT.into(), Some("fedora".into())));
                });
                key
            })
            .unwrap();
        let moved = key.clone();
        settle(cx, window, |shell, cx| {
            shell
                .workspace
                .active
                .is_some_and(|id| id != blank && shell.sessions[&id].read(cx).summary.cwd == moved)
                && !shell.sessions.contains_key(&blank)
        });
        window
            .update(cx, |shell, _, cx| {
                let active = shell.workspace.active.unwrap();
                let view = shell.sessions[&active].read(cx);
                assert_eq!(view.input.read(cx).content, "Run this on Fedora");
                assert_eq!(view.settings.model.as_deref(), Some("picked-model"));
                assert_eq!(view.settings.effort.as_deref(), Some("high"));
                assert!(!shell.workspace.tabs.contains(&blank));
                // Choosing This Mac moves it back, still without losing the draft.
                shell.sessions[&active].update(cx, |_, cx| {
                    cx.emit(ProjectHostSelected(PROJECT.into(), None));
                });
            })
            .unwrap();
        settle(cx, window, |shell, cx| {
            shell
                .workspace
                .active
                .is_some_and(|id| shell.sessions[&id].read(cx).summary.cwd == PROJECT)
        });
        window
            .update(cx, |shell, _, cx| {
                let active = shell.workspace.active.unwrap();
                let view = shell.sessions[&active].read(cx);
                assert_eq!(view.input.read(cx).content, "Run this on Fedora");
                assert_eq!(view.settings.model.as_deref(), Some("picked-model"));
                assert_eq!(view.settings.effort.as_deref(), Some("high"));
            })
            .unwrap();
    }

    #[gpui::test]
    fn choosing_a_host_leaves_a_started_session_where_it_runs(cx: &mut TestAppContext) {
        let (window, _, blank) = super::super::tests::window(cx);
        window
            .update(cx, |shell, _, cx| {
                connect_fedora(shell);
                shell.sessions[&blank].update(cx, |view, cx| {
                    view.summary.has_messages = true;
                    cx.emit(ProjectHostSelected(PROJECT.into(), Some("fedora".into())));
                });
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(shell.workspace.active, Some(blank));
                assert_eq!(shell.sessions[&blank].read(cx).summary.cwd, PROJECT);
            })
            .unwrap();
    }
}
