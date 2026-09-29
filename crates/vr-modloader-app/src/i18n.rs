//! Tiny i18n: the UI is written in English (the key IS the English text) and [`tr`] looks the Spanish up in
//! [`es`]. Placeholders are `{}` (filled in order by [`trf`]). A test checks that every `tr("…")` / `trf("…")`
//! literal of the UI has a Spanish entry, so adding a string without translating it fails `cargo test`.

use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Es,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
        }
    }
    pub fn from_code(s: &str) -> Lang {
        if s.trim().to_ascii_lowercase().starts_with("es") {
            Lang::Es
        } else {
            Lang::En
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Es => "Español",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(l: Lang) {
    LANG.store(if l == Lang::Es { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::Es
    } else {
        Lang::En
    }
}

/// The text in the current language (the English key when there is no translation).
pub fn tr(key: &'static str) -> &'static str {
    match lang() {
        Lang::En => key,
        Lang::Es => es(key).unwrap_or(key),
    }
}

/// [`tr`] with `{}` placeholders filled in order.
pub fn trf(key: &'static str, args: &[&dyn Display]) -> String {
    fill(tr(key), args)
}

pub fn fill(pattern: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut it = args.iter();
    let mut rest = pattern;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        match it.next() {
            Some(a) => out.push_str(&a.to_string()),
            None => out.push_str("{}"),
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// Spanish table.
pub fn es(key: &str) -> Option<&'static str> {
    Some(match key {
        // ---- tabs, header
        "Manager" => "Gestor",
        "Studio" => "Studio",
        "Settings" => "Ajustes",
        "Game folder" => "Carpeta del juego",
        "Change…" => "Cambiar…",
        "Detect" => "Detectar",
        "No game folder selected" => "No hay carpeta del juego",
        "Game not found. Choose the folder that contains nie.exe." => "No se encuentra el juego. Elige la carpeta que contiene nie.exe.",
        "That folder is not the game (no nie.exe / data\\cpk_list.cfg.bin)." => "Esa carpeta no es la del juego (no hay nie.exe / data\\cpk_list.cfg.bin).",
        "v7.1.2 (OK)" => "v7.1.2 (OK)",
        "Not v7.1.2: the ModLoader stays inactive on this build" => "No es la v7.1.2: el ModLoader no se activa con esta versión",
        "checking version…" => "comprobando versión…",
        "The game is running: changes apply at the next start." => "El juego está abierto: los cambios se aplican al volver a arrancarlo.",
        "(command-line override, not saved)" => "(indicada por línea de órdenes, no se guarda)",
        // ---- ModLoader card
        "ModLoader" => "ModLoader",
        "Not installed" => "No instalado",
        "Installed {}" => "Instalado {}",
        "Installed (unknown version)" => "Instalado (versión desconocida)",
        "Outdated: a mod needs {}" => "Desactualizado: un mod pide {}",
        "Update available: {}" => "Actualización disponible: {}",
        "Install" => "Instalar",
        "Update" => "Actualizar",
        "Reinstall" => "Reinstalar",
        "Remove" => "Quitar",
        "Install ModLoader from file…" => "Instalar el ModLoader desde archivo…",
        "No ModLoader package found (embedded, modloader\\ or modloader.zip next to the exe)." => "No hay paquete del ModLoader (incluido, modloader\\ o modloader.zip junto al exe).",
        "Package: {}" => "Paquete: {}",
        "installed by another tool: remove it with that tool" => "instalado por otra herramienta: quítalo con ella",
        "The mods module is off in evt_loader\\config.toml: mods are ignored." => "El módulo mods está apagado en evt_loader\\config.toml: los mods se ignoran.",
        "Turn on" => "Encender",
        "Remove the ModLoader? Its backup puts back the files it replaced. Your mods folder is kept." => "¿Quitar el ModLoader? Su copia de seguridad devuelve los archivos que sustituyó. La carpeta de mods se conserva.",
        // ---- toolbar
        "Save" => "Guardar",
        "Saved." => "Guardado.",
        "Launch game" => "Jugar",
        "Install mod…" => "Instalar mod…",
        "Refresh" => "Recargar",
        "Open mods folder" => "Abrir carpeta de mods",
        "Unsaved changes" => "Cambios sin guardar",
        "Profile" => "Perfil",
        "Default" => "Predeterminado",
        "New…" => "Nuevo…",
        "Rename…" => "Renombrar…",
        "Delete" => "Borrar",
        "New profile" => "Nuevo perfil",
        "Rename profile" => "Renombrar perfil",
        "Name" => "Nombre",
        "Create" => "Crear",
        "Rename" => "Renombrar",
        "Delete profile «{}»?" => "¿Borrar el perfil «{}»?",
        "Drop a .zip here to install it" => "Suelta un .zip aquí para instalarlo",
        // ---- list
        "Mods" => "Mods",
        "On" => "Act.",
        "Version" => "Versión",
        "Author" => "Autor",
        "Status" => "Estado",
        "Top = highest priority (loads last, wins conflicts). Drag a row to reorder." => "Arriba = más prioridad (carga la última y gana los conflictos). Arrastra una fila para ordenarla.",
        "No mods installed. Use «Install mod…» or drop a .zip on the window." => "No hay mods. Usa «Instalar mod…» o suelta un .zip en la ventana.",
        "OK" => "OK",
        "Off" => "Apagado",
        "Not loaded" => "No se carga",
        "Warning" => "Aviso",
        "Move up" => "Subir",
        "Move down" => "Bajar",
        "Enable all" => "Activar todos",
        "Disable all" => "Desactivar todos",
        // ---- details
        "Select a mod to see its details." => "Elige un mod para ver sus detalles.",
        "by {}" => "de {}",
        "Folder" => "Carpeta",
        "Requires" => "Necesita",
        "Provides" => "Ofrece",
        "Incompatible with" => "Incompatible con",
        "Needs ModLoader" => "Necesita ModLoader",
        "Plugin" => "Plugin",
        "Tags" => "Etiquetas",
        "Updated" => "Actualizado",
        "Contents" => "Contenido",
        "{} files, {} Lua scripts, {} data deltas" => "{} archivos, {} scripts Lua, {} deltas de datos",
        "Uninstall…" => "Desinstalar…",
        "Open folder" => "Abrir carpeta",
        "Problems" => "Problemas",
        "Not loaded: {}" => "No se carga: {}",
        "requires «{}», which is not installed" => "necesita «{}», que no está instalado",
        "requires «{}», which is disabled" => "necesita «{}», que está desactivado",
        "requires «{}» {} (installed: {})" => "necesita «{}» {} (instalado: {})",
        "incompatible with «{}» (enabled)" => "incompatible con «{}» (activado)",
        "shares {} file(s)/cell(s) with «{}» — this mod wins" => "comparte {} archivo(s)/celda(s) con «{}»: gana este mod",
        "shares {} file(s)/cell(s) with «{}» — «{}» wins" => "comparte {} archivo(s)/celda(s) con «{}»: gana «{}»",
        "second folder with the id «{}»: ignored" => "segunda carpeta con el id «{}»: se ignora",
        "needs ModLoader {} (installed: {})" => "necesita el ModLoader {} (instalado: {})",
        "Install missing…" => "Instalar lo que falta…",
        // ---- bottom panel
        "Conflicts" => "Conflictos",
        "Log" => "Registro",
        "No conflicts between the enabled mods." => "No hay conflictos entre los mods activados.",
        "No problems." => "Sin problemas.",
        "What" => "Qué",
        "Mods (load order)" => "Mods (orden de carga)",
        "Winner" => "Gana",
        "file" => "archivo",
        "cell" => "celda",
        "new row" => "fila nueva",
        "Filter" => "Filtro",
        "{} conflicts" => "{} conflictos",
        "Audio declarations (audio.toml): the mod loaded last is expected to win." => "Declaraciones de audio (audio.toml): se espera que gane el último mod cargado.",
        // ---- install flow
        "Install mods" => "Instalar mods",
        "Reading {}…" => "Leyendo {}…",
        "Downloading {}…" => "Descargando {}…",
        "Installing…" => "Instalando…",
        "Working…" => "Trabajando…",
        "Cancel" => "Cancelar",
        "Close" => "Cerrar",
        "Yes" => "Sí",
        "No" => "No",
        "Continue" => "Continuar",
        "Install selected" => "Instalar los elegidos",
        "new" => "nuevo",
        "upgrade from {}" => "actualiza la {}",
        "downgrade from {}" => "baja de la {}",
        "same version {} (reinstall)" => "misma versión {} (reinstalar)",
        "The installed copy is moved to mods\\_trash\\ first." => "La copia instalada se mueve antes a mods\\_trash\\.",
        "Errors: this mod cannot be installed" => "Errores: este mod no se puede instalar",
        "Warnings" => "Avisos",
        "No mod.toml found in the archive." => "El archivo no contiene ningún mod.toml.",
        "Installed: {}" => "Instalado: {}",
        "Missing dependencies" => "Faltan dependencias",
        "These mods need other mods that are not installed:" => "Estos mods necesitan otros mods que no están instalados:",
        "needed by {}" => "lo necesita {}",
        "Download and install" => "Descargar e instalar",
        "Open page" => "Abrir página",
        "Install from file…" => "Instalar desde archivo…",
        "Later" => "Más tarde",
        "Uninstall «{}»?" => "¿Desinstalar «{}»?",
        "The folder is moved to mods\\_trash\\ (you can restore it by moving it back)." => "La carpeta se mueve a mods\\_trash\\ (se recupera volviéndola a mover).",
        "These enabled mods need it: {}" => "Estos mods activados lo necesitan: {}",
        "Uninstall" => "Desinstalar",
        "Moved to {}" => "Movido a {}",
        "Unsaved changes: save before switching profile?" => "Hay cambios sin guardar: ¿guardarlos antes de cambiar de perfil?",
        "Save and switch" => "Guardar y cambiar",
        "Discard and switch" => "Descartar y cambiar",
        "Unsaved changes: save before launching?" => "Hay cambios sin guardar: ¿guardarlos antes de jugar?",
        "Save and launch" => "Guardar y jugar",
        "Error" => "Error",
        "Done" => "Hecho",
        // ---- 1-click
        "Install from link" => "Instalar desde enlace",
        "A website asks to download and install a mod from:" => "Una web pide descargar e instalar un mod desde:",
        "Only continue if you trust this site. The archive is checked before anything is installed." => "Continúa solo si te fías de la web. El archivo se revisa antes de instalar nada.",
        "Download" => "Descargar",
        "Invalid link: {}" => "Enlace no válido: {}",
        // ---- settings
        "Language" => "Idioma",
        "1-click install (GameBanana / Nexus style links)" => "Instalación con un clic (enlaces tipo GameBanana / Nexus)",
        "Registers the vrmodloader: link type for your Windows user (HKCU\\Software\\Classes\\vrmodloader), so «1-click install» buttons on mod sites open this program. Nothing is registered without this button." => "Registra el tipo de enlace vrmodloader: para tu usuario de Windows (HKCU\\Software\\Classes\\vrmodloader), para que los botones «1-click install» de las webs de mods abran este programa. Sin este botón no se registra nada.",
        "Register 1-click links" => "Registrar los enlaces de un clic",
        "Unregister" => "Quitar el registro",
        "Status: registered to this program" => "Estado: registrado a este programa",
        "Status: registered to another program: {}" => "Estado: registrado a otro programa: {}",
        "Status: not registered" => "Estado: sin registrar",
        "Registered." => "Registrado.",
        "Unregistered." => "Registro quitado.",
        "Trash" => "Papelera",
        "{} item(s) in mods\\_trash\\" => "{} elemento(s) en mods\\_trash\\",
        "Open trash" => "Abrir papelera",
        "Empty trash…" => "Vaciar papelera…",
        "Permanently delete everything in mods\\_trash\\? This cannot be undone." => "¿Borrar para siempre todo lo que hay en mods\\_trash\\? No se puede deshacer.",
        "Delete permanently" => "Borrar para siempre",
        "About" => "Acerca de",
        "Mod manager for the VR-ModLoader (Inazuma Eleven Victory Road PC v7.1.2). Free software under the GPL-3.0." => "Gestor de mods del VR-ModLoader (Inazuma Eleven Victory Road PC v7.1.2). Software libre bajo la GPL-3.0.",
        // ---- studio
        "Studio (coming soon)" => "Studio (próximamente)",
        "Create and edit mods with forms: characters, techniques, teams, texts… It reads your own game data (v7.1.2) to build a local index, so you pick game things from lists instead of typing ids. Nothing from the game ships with this program." => "Crear y editar mods con formularios: personajes, técnicas, equipos, textos… Lee los datos de tu propio juego (v7.1.2) para crear un índice local, así eliges las cosas del juego de listas en lugar de escribir ids. Este programa no incluye nada del juego.",
        "Game index" => "Índice del juego",
        "The index is built once from your game (a few minutes; read only) and stored in %LOCALAPPDATA%\\VR-ModLoader\\index." => "El índice se crea una vez a partir de tu juego (unos minutos; solo lectura) y se guarda en %LOCALAPPDATA%\\VR-ModLoader\\index.",
        "Open / build the index" => "Abrir / crear el índice",
        "Reading your game data…" => "Leyendo los datos de tu juego…",
        "Game index ready: {} entries{}" => "Índice del juego listo: {} entradas{}",
        " (built now)" => " (creado ahora)",
        "Search names or ids (any language)" => "Busca nombres o ids (cualquier idioma)",
        "All" => "Todo",
        "{} entries" => "{} entradas",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_placeholders() {
        assert_eq!(fill("a {} b {}", &[&1, &"x"]), "a 1 b x");
        assert_eq!(fill("{} {}", &[&1]), "1 {}");
        assert_eq!(fill("none", &[&1]), "none");
    }

    /// Every `tr("…")` / `trf("…"` literal in the UI sources has a Spanish entry with the same number of `{}`.
    #[test]
    fn every_ui_string_is_translated() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing = Vec::new();
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "rs") || p.file_name().is_some_and(|f| f == "i18n.rs") {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            for pat in ["tr(\"", "trf(\""] {
                let mut rest = src.as_str();
                while let Some(i) = rest.find(pat) {
                    // skip identifiers ending in tr / trf (e.g. `str(`)
                    let prev = rest[..i].chars().last();
                    rest = &rest[i + pat.len()..];
                    if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    let end = find_end(rest);
                    let lit = unescape(&rest[..end]);
                    n += 1;
                    match es(&lit) {
                        None => missing.push(format!("{}: {lit}", p.file_name().unwrap().to_string_lossy())),
                        Some(t) if t.matches("{}").count() != lit.matches("{}").count() => missing.push(format!("placeholders differ: {lit}")),
                        _ => {}
                    }
                }
            }
        }
        assert!(n > 50, "found only {n} UI strings");
        assert!(missing.is_empty(), "untranslated:\n{}", missing.join("\n"));
    }

    fn find_end(s: &str) -> usize {
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 2,
                b'"' => return i,
                _ => i += 1,
            }
        }
        b.len()
    }

    fn unescape(s: &str) -> String {
        let mut o = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('n') => o.push('\n'),
                    Some(x) => o.push(x),
                    None => {}
                }
            } else {
                o.push(c);
            }
        }
        o
    }
}
