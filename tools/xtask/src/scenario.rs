//! Escenarios de test QEMU declarados en `tests/qemu/*.toml`.

use crate::{root, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Solo tests unitarios de host (segundos).
    Unit,
    /// Unit + el arranque nominal en QEMU. Nivel por defecto y mínimo para `develop`.
    Smoke,
    /// Todos los escenarios QEMU, incluidos fallos provocados. Obligatorio hacia `main`.
    Full,
}

impl std::str::FromStr for Level {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "unit" => Ok(Self::Unit),
            "smoke" => Ok(Self::Smoke),
            "full" => Ok(Self::Full),
            other => Err(format!("nivel desconocido '{other}' (unit|smoke|full)")),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inject {
    /// Recorta `kernel.elf` a N bytes antes de arrancar.
    pub truncate_kernel: Option<usize>,
    /// Elimina `kernel.elf` de la ESP.
    #[serde(default)]
    pub remove_kernel: bool,
    /// Sobrescribe `e_entry` del kernel con este valor.
    pub patch_entry: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Código de salida de QEMU esperado. `isa-debug-exit` devuelve
    /// `(valor << 1) | 1`: el sistema escribe 0x10 en éxito (33) y 0x11 en
    /// fallo reportado por AriBoot o AriKernel (35).
    pub exit: Option<i32>,
    #[serde(default)]
    pub serial_contains: Vec<String>,
    #[serde(default)]
    pub serial_absent: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub level: Level,
    pub areas: Vec<String>,
    /// Contenido de `aricodeos/cmdline.txt`. `qemu_exit` hace que el sistema
    /// termine QEMU en vez de quedarse en `hlt`.
    #[serde(default = "default_cmdline")]
    pub cmdline: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub inject: Inject,
    pub expect: Expect,
    #[serde(skip)]
    pub file: PathBuf,
}

/// Un nombre de escenario también es un nombre de directorio bajo
/// `build/scenarios/`: solo `[a-z0-9._-]`, sin empezar por `.`, sin `..`.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(format!("nombre de escenario invalido '{name}': usar [a-z0-9._-], sin '/', sin '..', sin '.' inicial"))
    }
}

fn default_cmdline() -> String {
    "qemu_exit".into()
}
fn default_timeout() -> u64 {
    30
}

pub fn scenarios_dir() -> PathBuf {
    root().join("tests/qemu")
}

/// Carga todos los escenarios ordenados por nombre. Un TOML inválido es error:
/// un test mal escrito no debe pasar en silencio.
pub fn load_all() -> Result<Vec<Scenario>> {
    let dir = scenarios_dir();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|x| x == "toml") {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut s: Scenario =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if s.level == Level::Unit {
                return Err(format!(
                    "{}: un escenario QEMU no puede ser de nivel unit",
                    path.display()
                ));
            }
            s.file = path;
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

impl Scenario {
    pub fn matches(&self, max_level: Level, areas: &[String]) -> bool {
        self.level <= max_level
            && (areas.is_empty() || self.areas.iter().any(|a| areas.contains(a)))
    }
}

/// Selecciona escenarios por nivel y áreas. Para smoke/full, una selección
/// vacía es un error: un filtro mal escrito no debe dar un éxito sin arrancar
/// nada.
pub fn select<'a>(
    all: &'a [Scenario],
    level: Level,
    areas: &[String],
) -> Result<Vec<&'a Scenario>> {
    let selected: Vec<&Scenario> = all.iter().filter(|s| s.matches(level, areas)).collect();
    if selected.is_empty() && level > Level::Unit {
        let mut known: Vec<&str> = all
            .iter()
            .flat_map(|s| s.areas.iter().map(String::as_str))
            .collect();
        known.sort_unstable();
        known.dedup();
        let higher: Vec<&str> = all
            .iter()
            .filter(|s| s.level > level && s.areas.iter().any(|a| areas.contains(a)))
            .map(|s| s.name.as_str())
            .collect();
        let mut msg = format!(
            "ningun escenario QEMU coincide con nivel {level:?} y areas [{}]. Areas conocidas: {}",
            areas.join(","),
            known.join(", ")
        );
        if !higher.is_empty() {
            msg.push_str(&format!(". Con --all entrarian: {}", higher.join(", ")));
        }
        return Err(msg);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_scenarios_are_valid_and_unique() {
        let all = load_all().expect("tests/qemu/*.toml deben parsear");
        assert!(!all.is_empty());
        let mut names: Vec<_> = all.iter().map(|s| s.name.as_str()).collect();
        names.dedup();
        assert_eq!(names.len(), all.len(), "nombres duplicados");
        assert!(
            all.iter().any(|s| s.level == Level::Smoke),
            "debe existir al menos un smoke"
        );
        for s in &all {
            assert!(!s.areas.is_empty(), "{}: sin areas", s.name);
        }
    }

    fn scenario(name: &str, level: &str, areas: &str) -> Scenario {
        toml::from_str(&format!(
            "name = \"{name}\"\nlevel = \"{level}\"\nareas = [{areas}]\n[expect]\nexit = 33\n"
        ))
        .unwrap()
    }

    #[test]
    fn empty_selection_is_an_error_for_qemu_levels() {
        let all = vec![
            scenario("boot.ok", "smoke", "\"boot\""),
            scenario("boot.panic", "full", "\"boot\", \"panic\""),
        ];
        // area inexistente (auditoria, hallazgo 5)
        let err = select(&all, Level::Smoke, &["booot".into()]).unwrap_err();
        assert!(
            err.contains("booot") && err.contains("boot, panic"),
            "{err}"
        );
        // area cuyo unico escenario es full
        let err = select(&all, Level::Smoke, &["panic".into()]).unwrap_err();
        assert!(err.contains("Con --all entrarian: boot.panic"), "{err}");
        // en unit la seleccion vacia es normal
        assert!(select(&all, Level::Unit, &["booot".into()])
            .unwrap()
            .is_empty());
        assert_eq!(select(&all, Level::Full, &[]).unwrap().len(), 2);
    }

    #[test]
    fn scenario_names_cannot_escape_scenarios_dir() {
        for bad in [
            "/tmp/x",
            "../x",
            "a/b",
            "a\\b",
            ".hidden",
            "",
            "Boot.Ready",
            "a b",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} deberia rechazarse");
        }
        for good in ["boot.kernel_ready", "memory.oom-2", "x"] {
            assert!(validate_name(good).is_ok(), "{good:?} deberia aceptarse");
        }
    }

    #[test]
    fn level_filter_is_inclusive() {
        let s: Scenario = toml::from_str(
            r#"
            name = "x"
            level = "full"
            areas = ["boot"]
            [expect]
            exit = 35
            "#,
        )
        .unwrap();
        assert!(s.matches(Level::Full, &[]));
        assert!(!s.matches(Level::Smoke, &[]));
        assert!(s.matches(Level::Full, &["boot".into()]));
        assert!(!s.matches(Level::Full, &["memory".into()]));
        assert_eq!(s.cmdline, "qemu_exit");
        assert_eq!(s.timeout_secs, 30);
    }
}
