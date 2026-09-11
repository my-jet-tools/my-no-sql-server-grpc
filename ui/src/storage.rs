const THEME_KEY: &str = "mns_theme";

/// Namespace the UI is currently pointed at. Absent means the default namespace,
/// which is what the server assumes when a request carries no `ns` header.
const NAMESPACE_KEY: &str = "mns_namespace";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// The api layer reads this on every request, so a page reload restores the
/// selected namespace without anything having to re-apply it.
pub fn load_namespace() -> Option<String> {
    let s = local_storage()?;
    s.get_item(NAMESPACE_KEY).ok()?.filter(|v| !v.is_empty())
}

pub fn save_namespace(namespace: &str) {
    if let Some(s) = local_storage() {
        // The default namespace is stored as "nothing selected": that way the UI
        // sends no header at all and behaves exactly like it did before
        // namespaces existed.
        if namespace.is_empty() {
            let _ = s.remove_item(NAMESPACE_KEY);
        } else {
            let _ = s.set_item(NAMESPACE_KEY, namespace);
        }
    }
}

pub fn save_theme(theme: &str) {
    if let Some(s) = local_storage() {
        let _ = s.set_item(THEME_KEY, theme);
    }
}

pub fn load_theme() -> Option<String> {
    let s = local_storage()?;
    s.get_item(THEME_KEY).ok()?.filter(|v| !v.is_empty())
}

pub fn apply_theme(theme: &str) {
    if let Some(window) = web_sys::window()
        && let Some(doc) = window.document()
        && let Some(html) = doc.document_element()
    {
        let _ = html.set_attribute("data-theme", theme);
    }
}
