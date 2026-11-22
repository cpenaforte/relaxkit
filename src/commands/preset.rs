//! `relaxkit preset lean|balanced|careful` (spec 6.2). The preset sets its tiers and its voice. Tiers set by hand are
//! cleared so the preset's tiers apply, and pins stay (spec 6.1, rule 5). A preset is global; `--project` does not
//! change it.

use anyhow::Result;

use crate::commands::config::{apply_roles, refuse_tier_without_list};
use crate::config::KitConfig;
use crate::config_apply::voice;
use crate::ctx::Ctx;
use crate::host::{HostKind, detect_hosts};
use crate::presets::Preset;
use crate::roles::{Role, RoleSetting};
use crate::state::{RunId, with_run_lock};

/// Applies `preset` to `config.toml` and to each detected host's roles and voice.
pub fn run(ctx: &Ctx, preset: Preset) -> Result<()> {
    let body = |run: &RunId| -> Result<()> {
        let mut config = KitConfig::load(&ctx.dirs)?;
        config.preset = preset;
        config.voice = None;
        keep_pins(&mut config);
        refuse_tier_without_list(
            ctx,
            &detect_hosts(ctx),
            &[HostKind::Claude, HostKind::Opencode],
            &config,
            &Role::ALL,
        )?;
        config.save(ctx, run)?;
        voice::apply(ctx, run, preset.voice(), false)?;
        apply_roles(ctx, run, &detect_hosts(ctx), &config, None)?;
        ctx.out.info(&format!(
            "preset {} with voice {}; pinned roles keep their pins",
            preset.as_str(),
            preset.voice().as_str()
        ));
        Ok(())
    };
    if ctx.dry_run {
        return body(&ctx.run);
    }
    with_run_lock(&ctx.dirs, || body(&ctx.run))
}

/// Drops every tier, shared or host-only, and keeps the pins.
fn keep_pins(config: &mut KitConfig) {
    config
        .roles
        .retain(|_, setting| matches!(setting, RoleSetting::Pin(_)));
    for roles in config.host_roles.values_mut() {
        roles.retain(|_, setting| matches!(setting, RoleSetting::Pin(_)));
    }
    config.host_roles.retain(|_, roles| !roles.is_empty());
}
