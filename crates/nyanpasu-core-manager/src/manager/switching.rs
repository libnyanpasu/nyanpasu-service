use crate::{
    config::{
        ConfigSnapshot,
        diff::{self, OverlapBlock},
    },
    error::Error,
    instance::Instance,
    kind::CoreKind,
    probe::ProbePhase,
    spec::{ControllerMode, InstanceSpec, ResolvedController},
    state::{ConfigRevision, CoreState},
};

use super::{
    Active, CoreManager, Ctrl, DegradeReason, PreparedGraceful, PreparedLaunch, SwitchOutcome,
    abort_and_await,
    publish::spec_summary,
    quarantine::{record_quarantine, reject_quarantine},
    spawn_forwarder,
};

fn graceful_degrade_reason(
    managed: bool,
    kind: CoreKind,
    overlap_block: Option<OverlapBlock>,
) -> Option<DegradeReason> {
    if !managed {
        return Some(DegradeReason::PassthroughMode);
    }
    if !matches!(kind, CoreKind::Mihomo) {
        return Some(DegradeReason::UnsupportedKind);
    }
    if let Some(block) = overlap_block {
        return Some(match block {
            OverlapBlock::DnsListen => DegradeReason::DnsListen,
            OverlapBlock::InboundSurface => DegradeReason::InboundConflict,
        });
    }
    None
}

impl CoreManager {
    pub async fn restart(&self) -> Result<SwitchOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        reject_quarantine(&ctrl)?;
        let spec = ctrl.last_spec.clone().ok_or(Error::NotStarted)?;
        self.switch_locked(&mut ctrl, spec).await
    }

    pub async fn switch(&self, spec: InstanceSpec) -> Result<SwitchOutcome, Error> {
        let mut ctrl = self.inner.ctrl.lock().await;
        reject_quarantine(&ctrl)?;
        self.switch_locked(&mut ctrl, spec).await
    }

    async fn switch_locked(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
    ) -> Result<SwitchOutcome, Error> {
        let running = ctrl
            .current
            .as_ref()
            .is_some_and(|active| !active.instance.state().borrow().state.is_terminal());
        if !running {
            if let Some(stale) = ctrl.current.take() {
                abort_and_await(stale.forwarder).await;
                let epoch = stale.instance.epoch();
                if let Err(error) = stale
                    .instance
                    .stop_and_confirm_dead(self.inner.options.stop_timeout)
                    .await
                {
                    if matches!(error, Error::StopUnconfirmed(_)) {
                        return Err(self.latch_quarantine(ctrl, epoch, error));
                    }
                    return Err(error);
                }
                self.inner.store.cleanup_epoch(epoch).await?;
            }
            self.start_locked(ctrl, spec).await?;
            return Ok(SwitchOutcome::Hard {
                reason: DegradeReason::NotRunning,
            });
        }

        let snapshot = ConfigSnapshot::load(&spec.config_path).await?;
        let managed = matches!(
            self.inner.options.controller_mode,
            ControllerMode::Managed { .. }
        );
        match graceful_degrade_reason(
            managed,
            spec.core.kind,
            diff::overlap_block(snapshot.document()),
        ) {
            Some(reason) => {
                self.hard_switch(ctrl, spec, snapshot).await?;
                Ok(SwitchOutcome::Hard { reason })
            }
            None => self.graceful_switch(ctrl, spec, snapshot).await,
        }
    }

    async fn hard_switch(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
        snapshot: ConfigSnapshot,
    ) -> Result<(), Error> {
        let epoch = self.next_epoch();
        let prepared = match self.prepare_launch(&spec, epoch, &snapshot).await {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        let old_epoch = ctrl.current.as_ref().map(|active| active.instance.epoch());
        self.inner.publish(
            CoreState::Switching {
                from: old_epoch,
                to: epoch,
            },
            Some(spec_summary(&prepared.source_spec)),
            Some(prepared.controller.host.clone()),
            Some(prepared.revision.clone()),
        );

        let old = ctrl.current.take().expect("running checked by caller");
        abort_and_await(old.forwarder).await;
        let old_epoch = old.instance.epoch();
        if let Err(error) = old
            .instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            let _ = self.inner.store.cleanup_epoch(epoch).await;
            if matches!(error, Error::StopUnconfirmed(_)) {
                return Err(self.latch_quarantine(ctrl, old_epoch, error));
            }
            self.publish_terminal_error(&error);
            return Err(error);
        }
        if let Err(error) = self.inner.store.cleanup_epoch(old_epoch).await {
            self.publish_terminal_error(&error);
            return Err(error);
        }
        self.start_prepared(ctrl, prepared).await
    }

    async fn graceful_switch(
        &self,
        ctrl: &mut Ctrl,
        spec: InstanceSpec,
        snapshot: ConfigSnapshot,
    ) -> Result<SwitchOutcome, Error> {
        let old_epoch = ctrl.current.as_ref().map(|active| active.instance.epoch());
        let epoch = self.next_epoch();
        let prepared = match self.prepare_graceful(&spec, epoch, &snapshot).await {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        let PreparedGraceful {
            launch,
            full_staged,
            restoration,
        } = prepared;
        self.inner.publish(
            CoreState::Switching {
                from: old_epoch,
                to: epoch,
            },
            Some(spec_summary(&launch.source_spec)),
            Some(launch.controller.host.clone()),
            Some(launch.revision.clone()),
        );

        let instance = match self
            .spawn_instance(
                launch.effective_spec.clone(),
                epoch,
                launch.controller.clone(),
            )
            .await
        {
            Ok(instance) => instance,
            Err(error) => {
                drop(full_staged);
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.republish_retained(ctrl);
                return Err(error);
            }
        };
        if let Err(error) = instance.wait_ready().await {
            drop(full_staged);
            match instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                Ok(()) => {
                    let _ = self.inner.store.cleanup_epoch(epoch).await;
                    self.republish_retained(ctrl);
                    return Err(error);
                }
                Err(stop_error) => {
                    let error = Error::StopUnconfirmed(format!(
                        "{error}; failed to stop rejected graceful bootstrap: {stop_error}"
                    ));
                    return Err(self.latch_quarantine(ctrl, epoch, error));
                }
            }
        }

        let old = ctrl.current.take().expect("running checked by caller");
        abort_and_await(old.forwarder).await;
        let old_epoch = old.instance.epoch();
        if let Err(error) = old
            .instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            drop(full_staged);
            let old_uncertain = matches!(error, Error::StopUnconfirmed(_));
            let old_reason = error.to_string();
            let new_stop = instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await;
            match new_stop {
                Ok(()) => {
                    let _ = self.inner.store.cleanup_epoch(epoch).await;
                    if old_uncertain {
                        return Err(self.latch_quarantine(ctrl, old_epoch, error));
                    }
                    self.publish_terminal_error(&error);
                    return Err(error);
                }
                Err(new_error) => {
                    if old_uncertain {
                        record_quarantine(ctrl, old_epoch, old_reason);
                    }
                    let error = Error::StopUnconfirmed(format!(
                        "old epoch stop failed: {error}; new bootstrap stop also failed: {new_error}"
                    ));
                    return Err(self.latch_quarantine(ctrl, epoch, error));
                }
            }
        }

        let commit = match self.inner.store.commit_replace(full_staged, epoch).await {
            Ok(commit) => commit,
            Err(error) => {
                let new_stop = instance
                    .stop_and_confirm_dead(self.inner.options.stop_timeout)
                    .await;
                if new_stop.is_ok() {
                    let _ = self.inner.store.cleanup_epoch(epoch).await;
                }
                let error = match new_stop {
                    Ok(()) => error,
                    Err(new_error) => Error::StopUnconfirmed(format!(
                        "full runtime commit failed: {error}; bootstrap stop also failed: {new_error}"
                    )),
                };
                if matches!(error, Error::StopUnconfirmed(_)) {
                    return Err(self.latch_quarantine(ctrl, epoch, error));
                }
                self.publish_terminal_error(&error);
                return Err(error);
            }
        };
        let durability_warning = commit.durability_warning().map(str::to_owned);
        if let Some(warning) = durability_warning.as_deref() {
            tracing::warn!("graceful runtime replacement durability is uncertain: {warning}");
        }

        let reconciled = tokio::time::timeout(self.inner.options.reconcile_timeout, async {
            match restoration.as_ref() {
                Some((patch, projection)) => {
                    self.patch_and_verify(&instance, patch, projection).await
                }
                None => instance.probe_now(ProbePhase::Reconcile).await.is_healthy(),
            }
        })
        .await
        .unwrap_or(false);
        if reconciled {
            self.install_switched(ctrl, instance, launch);
            let result = self
                .inner
                .store
                .cleanup_epoch(old_epoch)
                .await
                .map(|()| SwitchOutcome::Graceful);
            return with_switch_durability_result(result, durability_warning);
        }

        let effective_spec = launch.effective_spec.clone();
        let controller = launch.controller.clone();
        if let Err(error) = instance
            .stop_and_confirm_dead(self.inner.options.stop_timeout)
            .await
        {
            let error = if matches!(error, Error::StopUnconfirmed(_)) {
                self.latch_quarantine(ctrl, epoch, error)
            } else {
                self.publish_terminal_error(&error);
                error
            };
            return with_switch_durability_result(Err(error), durability_warning);
        }
        let replacement = match self
            .spawn_replacement(effective_spec, epoch, controller)
            .await
        {
            Ok(replacement) => replacement,
            Err(error @ Error::StopUnconfirmed(_)) => {
                let error = self.latch_quarantine(ctrl, epoch, error);
                return with_switch_durability_result(Err(error), durability_warning);
            }
            Err(error) => {
                let _ = self.inner.store.cleanup_epoch(epoch).await;
                self.publish_terminal_error(&error);
                return with_switch_durability_result(Err(error), durability_warning);
            }
        };
        self.install_switched(ctrl, replacement, launch);
        let result =
            self.inner
                .store
                .cleanup_epoch(old_epoch)
                .await
                .map(|()| SwitchOutcome::Hard {
                    reason: DegradeReason::PatchFailed,
                });
        with_switch_durability_result(result, durability_warning)
    }

    fn install_switched(&self, ctrl: &mut Ctrl, instance: Instance, prepared: PreparedLaunch) {
        let epoch = prepared.revision.epoch;
        let pid = instance.pid().unwrap_or_default();
        self.inner.publish_instance(
            &instance,
            CoreState::Running { epoch, pid },
            &prepared.source_spec,
            &prepared.revision,
        );
        let forwarder = spawn_forwarder(&self.inner, instance.state(), epoch);
        ctrl.last_spec = Some(prepared.source_spec.clone());
        ctrl.current = Some(Active {
            instance,
            forwarder,
            source_spec: prepared.source_spec,
            revision: prepared.revision,
            source_document: prepared.source_document,
            effective_document: prepared.effective_document,
        });
    }

    pub(super) async fn prepare_launch(
        &self,
        spec: &InstanceSpec,
        epoch: u64,
        snapshot: &ConfigSnapshot,
    ) -> Result<PreparedLaunch, Error> {
        debug_assert_eq!(snapshot.source_path(), spec.config_path);
        if tokio::fs::metadata(&spec.core.binary_path).await.is_err() {
            return Err(Error::BinaryNotFound(spec.core.binary_path.clone()));
        }
        spec.core
            .kind
            .run_args(&spec.working_dir, &spec.config_path)?;
        let prepared = snapshot.prepare_full(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
        )?;
        let staged = self.inner.store.stage(epoch, &prepared.bytes).await?;

        let mut check_spec = spec.clone();
        check_spec.config_path = staged.path().to_owned();
        crate::kind::check_config(&check_spec).await?;

        let runtime_path = self.inner.store.commit_new(staged, epoch).await?;
        let mut effective_spec = spec.clone();
        effective_spec.config_path = runtime_path.clone();
        effective_spec.pid_file = Some(self.inner.store.pid_path(epoch));
        Ok(PreparedLaunch {
            source_spec: spec.clone(),
            effective_spec,
            controller: prepared.controller,
            revision: ConfigRevision {
                epoch,
                generation: 1,
                source_hash: prepared.source_hash,
                effective_hash: prepared.effective_hash,
                runtime_path,
            },
            source_document: snapshot.document().clone(),
            effective_document: prepared.document,
        })
    }

    async fn prepare_graceful(
        &self,
        spec: &InstanceSpec,
        epoch: u64,
        snapshot: &ConfigSnapshot,
    ) -> Result<PreparedGraceful, Error> {
        debug_assert_eq!(snapshot.source_path(), spec.config_path);
        if tokio::fs::metadata(&spec.core.binary_path).await.is_err() {
            return Err(Error::BinaryNotFound(spec.core.binary_path.clone()));
        }
        spec.core
            .kind
            .run_args(&spec.working_dir, &spec.config_path)?;
        let full = snapshot.prepare_full(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
        )?;
        let bootstrap = snapshot.prepare_bootstrap(
            &self.inner.options.controller_mode,
            self.inner.store.dir(),
            epoch,
        )?;
        if full.controller.host != bootstrap.controller.host
            || full.controller.secret != bootstrap.controller.secret
        {
            return Err(Error::InvalidConfig(
                "full and bootstrap configs resolved different controllers".into(),
            ));
        }
        let restoration = diff::restoration_patch(&bootstrap.document, &full.document)?;

        let full_staged = self.inner.store.stage(epoch, &full.bytes).await?;
        let mut check_spec = spec.clone();
        check_spec.config_path = full_staged.path().to_owned();
        crate::kind::check_config(&check_spec).await?;

        let bootstrap_staged = self.inner.store.stage(epoch, &bootstrap.bytes).await?;
        check_spec.config_path = bootstrap_staged.path().to_owned();
        crate::kind::check_config(&check_spec).await?;
        let runtime_path = self.inner.store.commit_new(bootstrap_staged, epoch).await?;

        let mut effective_spec = spec.clone();
        effective_spec.config_path = runtime_path.clone();
        effective_spec.pid_file = Some(self.inner.store.pid_path(epoch));
        Ok(PreparedGraceful {
            launch: PreparedLaunch {
                source_spec: spec.clone(),
                effective_spec,
                controller: full.controller,
                revision: ConfigRevision {
                    epoch,
                    generation: 1,
                    source_hash: full.source_hash,
                    effective_hash: full.effective_hash,
                    runtime_path,
                },
                source_document: snapshot.document().clone(),
                effective_document: full.document,
            },
            full_staged,
            restoration,
        })
    }

    pub(super) async fn spawn_replacement(
        &self,
        effective_spec: InstanceSpec,
        epoch: u64,
        controller: ResolvedController,
    ) -> Result<Instance, Error> {
        let instance = self
            .spawn_instance(effective_spec, epoch, controller)
            .await?;
        if let Err(error) = instance.wait_ready().await {
            return match instance
                .stop_and_confirm_dead(self.inner.options.stop_timeout)
                .await
            {
                Ok(()) => Err(error),
                Err(stop_error) => Err(Error::StopUnconfirmed(format!(
                    "{error}; failed to stop rejected replacement: {stop_error}"
                ))),
            };
        }
        Ok(instance)
    }
}

fn with_switch_durability_warning(
    outcome: SwitchOutcome,
    warning: Option<String>,
) -> SwitchOutcome {
    match warning {
        Some(warning) => SwitchOutcome::DurabilityUncertain {
            outcome: Box::new(outcome),
            warning,
        },
        None => outcome,
    }
}

pub(super) fn with_switch_durability_result(
    result: Result<SwitchOutcome, Error>,
    warning: Option<String>,
) -> Result<SwitchOutcome, Error> {
    match (result, warning) {
        (Ok(outcome), warning) => Ok(with_switch_durability_warning(outcome, warning)),
        (Err(error), Some(warning)) => Err(Error::DurabilityUncertain {
            source: Box::new(error),
            warning,
        }),
        (Err(error), None) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_matrix_matches_the_spec() {
        assert_eq!(
            graceful_degrade_reason(false, CoreKind::Mihomo, None),
            Some(DegradeReason::PassthroughMode)
        );
        assert_eq!(
            graceful_degrade_reason(true, CoreKind::ClashRs, None),
            Some(DegradeReason::UnsupportedKind)
        );
        assert_eq!(
            graceful_degrade_reason(true, CoreKind::Mihomo, Some(OverlapBlock::DnsListen)),
            Some(DegradeReason::DnsListen)
        );
        assert_eq!(graceful_degrade_reason(true, CoreKind::Mihomo, None), None);
    }
}
