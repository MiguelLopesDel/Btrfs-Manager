//! Minimal string catalog. Keys fall back to themselves so a missing
//! translation is visible in the UI instead of crashing.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiLanguage {
    En,
    PtBr,
}

pub(crate) fn ui_language() -> UiLanguage {
    let value = std::env::var("BTRFS_MANAGER_LANG")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if value.starts_with("pt") {
        UiLanguage::PtBr
    } else {
        UiLanguage::En
    }
}

/// (key, English, Portuguese) — every user-facing string in the app.
const TRANSLATIONS: &[(&str, &str, &str)] = &[
    ("snapshots", "Snapshots", "Snapshots"),
    ("refresh", "Refresh", "Atualizar"),
    (
        "cleanup_mounts",
        "Unmount temporary browse mounts",
        "Desmontar montagens temporárias",
    ),
    ("rollback_status", "Rollback status", "Status do rollback"),
    (
        "system_diagnostics",
        "System diagnostics",
        "Diagnósticos do sistema",
    ),
    (
        "snapshot_inventory",
        "Snapshot Inventory",
        "Inventário de snapshots",
    ),
    (
        "no_filesystem_selected",
        "No filesystem selected",
        "Nenhum filesystem selecionado",
    ),
    (
        "search_placeholder",
        "Search by name, tag, or date",
        "Buscar por nome, tag ou data",
    ),
    ("btrfs_filesystem", "Btrfs filesystem", "Filesystem Btrfs"),
    ("loading", "Loading", "Carregando"),
    (
        "no_btrfs_filesystems",
        "No Btrfs filesystems found",
        "Nenhum filesystem Btrfs encontrado",
    ),
    (
        "discovery_returned_no_mountpoints",
        "Discovery returned no mountpoints",
        "A descoberta não retornou pontos de montagem",
    ),
    (
        "no_discovery_data",
        "No filesystem discovery returned",
        "Nenhum dado de descoberta retornado",
    ),
    (
        "no_structured_data",
        "No structured data returned",
        "Nenhum dado estruturado retornado",
    ),
    (
        "read_filesystem_discovery",
        "Failed to read filesystem discovery",
        "Não foi possível ler a descoberta de filesystems",
    ),
    (
        "discover_filesystems",
        "Filesystem discovery failed",
        "Não foi possível descobrir filesystems Btrfs",
    ),
    (
        "read_inventory",
        "Failed to read inventory",
        "Não foi possível ler o inventário",
    ),
    (
        "unmount_temporary_mounts",
        "Failed to unmount temporary mounts",
        "Não foi possível desmontar montagens temporárias",
    ),
    (
        "cleanup_stale_mounts",
        "Failed to cleanup stale browse mounts",
        "Não foi possível limpar montagens temporárias antigas",
    ),
    (
        "browse_snapshot",
        "Failed to browse snapshot",
        "Não foi possível abrir o snapshot",
    ),
    (
        "unmount_snapshot",
        "Failed to unmount snapshot",
        "Não foi possível desmontar o snapshot",
    ),
    (
        "unlock_snapshot",
        "Failed to unlock snapshot",
        "Não foi possível desbloquear o snapshot",
    ),
    (
        "lock_snapshot",
        "Failed to lock snapshot",
        "Não foi possível bloquear o snapshot",
    ),
    (
        "delete_snapshot",
        "Failed to delete snapshot",
        "Não foi possível apagar o snapshot",
    ),
    (
        "delete_snapshots",
        "Failed to delete snapshots",
        "Não foi possível apagar os snapshots",
    ),
    ("select", "Select", "Selecionar"),
    (
        "select_tooltip",
        "Select multiple snapshots to delete",
        "Selecionar vários snapshots para apagar",
    ),
    ("cancel", "Cancel", "Cancelar"),
    ("delete_selected", "Delete selected", "Apagar selecionados"),
    (
        "create_snapshot",
        "Failed to create snapshot",
        "Não foi possível criar o snapshot",
    ),
    (
        "stage_rollback",
        "Failed to stage rollback",
        "Não foi possível preparar o rollback",
    ),
    (
        "stage_rollback_heading",
        "Restore this snapshot?",
        "Restaurar este snapshot?",
    ),
    (
        "stage_rollback_body",
        "Restore {name}?\n\nThis process will:\n• Save the current root as a return anchor\n• Replace the root subvolume with this snapshot's content\n\nThe system stays intact until the next reboot. The rollback can be reverted at any time (before or after rebooting).",
        "Restaurar {name}?\n\nEste processo irá:\n• Salvar o root atual como âncora de retorno\n• Substituir o subvolume root pelo conteúdo deste snapshot\n\nO sistema permanece intacto até o próximo reboot. O rollback pode ser revertido a qualquer momento (antes ou depois de reiniciar).",
    ),
    (
        "stage_rollback_confirm",
        "Restore and reboot",
        "Restaurar e reiniciar",
    ),
    (
        "stage_rollback_staged",
        "Rollback staged — reboot to activate. The app will offer a revert option on next start.",
        "Rollback preparado — reinicie para ativar. O app oferecerá opção de revert no próximo início.",
    ),
    (
        "read_rollback_state",
        "Failed to read rollback state",
        "Não foi possível ler o estado do rollback",
    ),
    (
        "keep_rollback",
        "Failed to keep rollback",
        "Não foi possível manter o rollback",
    ),
    (
        "revert_rollback",
        "Failed to revert rollback",
        "Não foi possível reverter o rollback",
    ),
    (
        "run_diagnostics",
        "Diagnostics failed",
        "Não foi possível executar diagnósticos",
    ),
    (
        "save_policy",
        "Failed to save policy",
        "Não foi possível salvar a política",
    ),
    (
        "preview_policy",
        "Failed to preview policy",
        "Não foi possível gerar a prévia",
    ),
    (
        "run_policy",
        "Failed to run policy",
        "Não foi possível executar a política",
    ),
    (
        "load_policy_logs",
        "Failed to load policy logs",
        "Não foi possível carregar os logs",
    ),
    (
        "service_hint",
        "The Btrfs Manager service is not available. Install the package and restart the service, or open Diagnostics to inspect the helper.",
        "O serviço do Btrfs Manager não está disponível. Instale o pacote e reinicie o serviço, ou abra Diagnósticos para verificar o helper.",
    ),
    (
        "polkit_hint",
        "Authorization was denied. Try again and confirm the password when Polkit asks.",
        "A autorização foi negada. Tente novamente e confirme a senha quando o Polkit pedir.",
    ),
    (
        "btrfs_subvolume_hint",
        "A path that must be a Btrfs subvolume exists as a regular directory. Open Diagnostics to find the path before continuing.",
        "Um caminho esperado como subvolume Btrfs existe como diretório comum. Abra Diagnósticos para ver o caminho e corrigir antes de continuar.",
    ),
    (
        "path_hint",
        "The path was rejected for safety. Use a path inside the Btrfs filesystem, without '..' or absolute paths where a relative Btrfs path is expected.",
        "O caminho foi rejeitado por segurança. Use um caminho dentro do filesystem Btrfs, sem '..' ou caminho absoluto onde o app espera caminho relativo.",
    ),
    (
        "systemctl_hint",
        "systemd rejected the operation. Check that the helper package is installed and units are loaded.",
        "O systemd recusou a operação. Verifique se o helper está instalado e se os units foram carregados.",
    ),
    (
        "generic_hint",
        "The operation did not complete. Open Diagnostics if it keeps failing.",
        "A operação não foi concluída. Abra Diagnósticos se o problema continuar.",
    ),
];

pub(crate) fn tr(key: &'static str) -> &'static str {
    let Some((_, en, pt)) = TRANSLATIONS.iter().find(|(k, _, _)| *k == key) else {
        return key;
    };
    match ui_language() {
        UiLanguage::PtBr => pt,
        UiLanguage::En => en,
    }
}

pub(crate) fn reconciled_message(count: usize) -> String {
    match ui_language() {
        UiLanguage::PtBr => {
            format!("{count} snapshot(s) removido(s) por fora foram reconciliados")
        }
        UiLanguage::En => format!("Reconciled {count} snapshot(s) deleted outside the app"),
    }
}
