/// Extracts a Steam app id from either plain text or a store URL such as
/// `https://store.steampowered.com/app/393380/Squad/`.
pub fn parse_app_id(text: &str) -> String {
    match text.split_once("/app/") {
        Some((_, after_app_segment)) => after_app_segment
            .chars()
            .take_while(char::is_ascii_digit)
            .collect(),
        None => text.chars().filter(char::is_ascii_digit).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_app_id;

    #[test]
    fn keeps_plain_digits() {
        assert_eq!(parse_app_id("620"), "620");
    }

    #[test]
    fn extracts_id_from_store_url_with_slug() {
        assert_eq!(
            parse_app_id("https://store.steampowered.com/app/393380/Squad/"),
            "393380"
        );
    }

    #[test]
    fn extracts_id_from_store_url_without_slug_or_with_query() {
        assert_eq!(parse_app_id("store.steampowered.com/app/440"), "440");
        assert_eq!(parse_app_id("https://x.com/app/440/?l=english"), "440");
    }

    #[test]
    fn strips_non_digits() {
        assert_eq!(parse_app_id("abc123"), "123");
        assert_eq!(parse_app_id(""), "");
    }
}
