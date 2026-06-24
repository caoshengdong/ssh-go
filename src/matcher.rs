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
    let ip_suffix: Vec<&Server> = servers.iter().filter(|s| s.host.ends_with(query)).collect();
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
    let ip_sub: Vec<&Server> = servers.iter().filter(|s| s.host.contains(query)).collect();
    ip_sub
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(alias: &str, host: &str) -> Server {
        Server {
            alias: alias.to_string(),
            host: host.to_string(),
            port: 22,
            user: "root".to_string(),
            auth: None,
        }
    }

    #[test]
    fn exact_alias_match_has_highest_priority() {
        let servers = vec![server("prod", "10.0.0.10"), server("prod-db", "prod")];

        let matched = match_servers(&servers, "prod");

        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].alias, "prod");
    }

    #[test]
    fn falls_back_through_suffix_alias_and_host_substring() {
        let servers = vec![
            server("web", "192.168.1.42"),
            server("prod-db", "10.0.0.15"),
            server("cache", "172.16.20.30"),
        ];

        assert_eq!(match_servers(&servers, ".42")[0].alias, "web");
        assert_eq!(match_servers(&servers, "DB")[0].alias, "prod-db");
        assert_eq!(match_servers(&servers, "16.20")[0].alias, "cache");
    }

    #[test]
    fn returns_empty_when_nothing_matches() {
        let servers = vec![server("prod", "10.0.0.10")];

        assert!(match_servers(&servers, "missing").is_empty());
    }
}
