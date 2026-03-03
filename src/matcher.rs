use crate::config::Server;

/// Match servers against a query string.
/// Priority (highest to lowest):
/// 1. Exact alias match
/// 2. IP suffix match (host ends with query)
/// 3. Alias substring match (case-insensitive)
/// 4. IP substring match
pub fn match_servers<'a>(servers: &'a [Server], query: &str) -> Vec<&'a Server> {
    let query_lower = query.to_lowercase();

    // 1. Exact alias match
    let exact: Vec<&Server> = servers
        .iter()
        .filter(|s| s.alias.to_lowercase() == query_lower)
        .collect();
    if !exact.is_empty() {
        return exact;
    }

    // 2. IP suffix match
    let ip_suffix: Vec<&Server> = servers
        .iter()
        .filter(|s| s.host.ends_with(query))
        .collect();
    if !ip_suffix.is_empty() {
        return ip_suffix;
    }

    // 3. Alias substring match (case-insensitive)
    let alias_sub: Vec<&Server> = servers
        .iter()
        .filter(|s| s.alias.to_lowercase().contains(&query_lower))
        .collect();
    if !alias_sub.is_empty() {
        return alias_sub;
    }

    // 4. IP substring match
    let ip_sub: Vec<&Server> = servers
        .iter()
        .filter(|s| s.host.contains(query))
        .collect();
    ip_sub
}
