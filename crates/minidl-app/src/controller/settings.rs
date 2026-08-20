use super::*;

impl Controller {
    pub(super) async fn apply_settings(
        &self,
        mut settings: SettingsUpdate,
    ) -> Result<(), AppError> {
        settings.proxy = settings.proxy.trim().to_owned();
        validate_label("proxy", &settings.proxy, 4_096, true)?;
        settings.max_concurrent = settings.max_concurrent.clamp(1, 20);
        settings.split = settings.split.clamp(1, 64);
        settings.connections = settings.connections.clamp(1, 16);
        settings.default_speed_limit = settings.default_speed_limit.max(0);
        if !matches!(settings.theme.as_str(), "system" | "light" | "dark") {
            return Err(AppError::classify("unsupported theme"));
        }
        if !matches!(
            settings.locale.as_str(),
            "system" | "en" | "tr" | "es" | "fr" | "de" | "ru" | "ar" | "zh" | "ja" | "ko"
        ) {
            return Err(AppError::classify("unsupported language"));
        }
        if !matches!(
            settings.on_complete_action.as_str(),
            "none" | "quit" | "sleep" | "shutdown"
        ) && !settings.on_complete_action.starts_with("run:")
        {
            return Err(AppError::classify("unsupported completion action"));
        }
        if settings.on_complete_action == "run:" {
            return Err(AppError::classify("completion command is empty"));
        }
        validate_label(
            "completion action",
            &settings.on_complete_action,
            4_096,
            false,
        )?;

        let old_bool = |key: &str, fallback: bool| -> Result<bool, AppError> {
            Ok(self
                .db
                .get_setting(key)?
                .map(|value| value == "true")
                .unwrap_or(fallback))
        };
        let old_autostart = old_bool("autostart", false)?;
        let old_magnets = old_bool("handle_magnets", true)?;
        let old_proxy = self.db.get_setting("proxy")?.unwrap_or_default();
        let old_maximum = self
            .db
            .get_setting("max_concurrent")?
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(5)
            .clamp(1, 20);
        let autostart_changed = settings.autostart != old_autostart;
        let magnets_changed = settings.handle_magnets != old_magnets;

        if autostart_changed {
            crate::platform::set_autostart(settings.autostart).map_err(AppError::classify)?;
        }
        if magnets_changed {
            if let Err(error) = crate::platform::set_magnet_handler(settings.handle_magnets) {
                if autostart_changed {
                    let _ = crate::platform::set_autostart(old_autostart);
                }
                return Err(AppError::classify(error));
            }
        }

        let engine = self.engine.current().await;
        if let Some(engine) = &engine {
            if let Err(error) = engine
                .rpc
                .change_global_option(json!({
                    "all-proxy": settings.proxy.clone(),
                    "max-concurrent-downloads": settings.max_concurrent.to_string(),
                }))
                .await
            {
                if magnets_changed {
                    let _ = crate::platform::set_magnet_handler(old_magnets);
                }
                if autostart_changed {
                    let _ = crate::platform::set_autostart(old_autostart);
                }
                return Err(error.into());
            }
        }

        let values = vec![
            ("proxy".into(), settings.proxy.clone()),
            ("max_concurrent".into(), settings.max_concurrent.to_string()),
            ("split".into(), settings.split.to_string()),
            ("max_conn".into(), settings.connections.to_string()),
            ("clipboard_watch".into(), settings.clipboard.to_string()),
            ("theme".into(), settings.theme.clone()),
            ("auto_organize".into(), settings.auto_organize.to_string()),
            (
                "prevent_duplicate_downloads".into(),
                settings.prevent_duplicates.to_string(),
            ),
            ("close_to_tray".into(), settings.close_to_tray.to_string()),
            ("autostart".into(), settings.autostart.to_string()),
            ("handle_magnets".into(), settings.handle_magnets.to_string()),
            ("dht_enabled".into(), settings.dht.to_string()),
            ("sandbox_children".into(), settings.sandbox.to_string()),
            (
                "block_private_ips".into(),
                settings.block_private.to_string(),
            ),
            ("locale".into(), settings.locale.clone()),
            (
                "on_complete_action".into(),
                settings.on_complete_action.clone(),
            ),
            (
                "on_complete_command_confirmed".into(),
                settings.on_complete_confirmed.to_string(),
            ),
            (
                "default_speed_limit".into(),
                settings.default_speed_limit.to_string(),
            ),
        ];
        if let Err(error) = self.db.set_settings(&values) {
            if let Some(engine) = &engine {
                let _ = engine
                    .rpc
                    .change_global_option(json!({
                        "all-proxy": old_proxy,
                        "max-concurrent-downloads": old_maximum.to_string(),
                    }))
                    .await;
            }
            if magnets_changed {
                let _ = crate::platform::set_magnet_handler(old_magnets);
            }
            if autostart_changed {
                let _ = crate::platform::set_autostart(old_autostart);
            }
            return Err(error.into());
        }

        {
            let mut defaults = self
                .defaults
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            defaults.split = settings.split;
            defaults.max_connection_per_server = settings.connections;
        }
        self.clipboard_on
            .store(settings.clipboard, Ordering::Relaxed);
        self.ytdlp
            .set_max_concurrent(settings.max_concurrent as usize);
        self.engine
            .update_runtime_options(
                settings.max_concurrent,
                (!settings.proxy.is_empty()).then_some(settings.proxy.clone()),
                settings.dht,
                settings.sandbox,
            )
            .await;
        self.power_fired.store(false, Ordering::Release);
        self.events
            .emit("settings:clipboard", json!({"enabled": settings.clipboard}));
        self.events.emit("settings:changed", json!({}));
        Ok(())
    }
}
