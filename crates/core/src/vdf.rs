//! Minimal parser for Valve's text KeyValues format (`.vdf` / `.acf`).
//!
//! Only what Steam's `libraryfolders.vdf` and `appmanifest_*.acf` need:
//! quoted strings, nested blocks, `//` comments and backslash escapes.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Block(Block),
}

/// Keys are stored lower-cased; Steam is inconsistent about key casing.
pub type Block = BTreeMap<String, Value>;

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Block(_) => None,
        }
    }

    pub fn as_block(&self) -> Option<&Block> {
        match self {
            Value::Block(b) => Some(b),
            Value::Str(_) => None,
        }
    }
}

/// Convenience: string value of `key` in `block`.
pub fn get_str<'a>(block: &'a Block, key: &str) -> Option<&'a str> {
    block.get(key).and_then(Value::as_str)
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Str(String),
    Open,
    Close,
}

fn tokenize(input: &str, escapes: bool) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                } else {
                    return Err("unexpected '/'".into());
                }
            }
            '{' => {
                chars.next();
                tokens.push(Token::Open);
            }
            '}' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None => return Err("unterminated string".into()),
                        Some('"') => break,
                        Some('\\') if escapes => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => return Err("unterminated escape".into()),
                        },
                        Some(other) => s.push(other),
                    }
                }
                tokens.push(Token::Str(s));
            }
            _ => {
                // Unquoted token (rare in Steam files, but legal).
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                tokens.push(Token::Str(s));
            }
        }
    }
    Ok(tokens)
}

fn parse_block(tokens: &[Token], pos: &mut usize, top_level: bool) -> Result<Block, String> {
    let mut block = Block::new();
    loop {
        match tokens.get(*pos) {
            None if top_level => return Ok(block),
            None => return Err("unexpected end of input inside block".into()),
            Some(Token::Close) if !top_level => {
                *pos += 1;
                return Ok(block);
            }
            Some(Token::Close) => return Err("unexpected '}'".into()),
            Some(Token::Open) => return Err("unexpected '{'".into()),
            Some(Token::Str(key)) => {
                *pos += 1;
                let value = match tokens.get(*pos) {
                    Some(Token::Str(v)) => {
                        *pos += 1;
                        Value::Str(v.clone())
                    }
                    Some(Token::Open) => {
                        *pos += 1;
                        Value::Block(parse_block(tokens, pos, false)?)
                    }
                    _ => return Err(format!("key `{key}` has no value")),
                };
                block.insert(key.to_ascii_lowercase(), value);
            }
        }
    }
}

/// Parse a whole document. The result maps the root key(s) to their values.
///
/// Valve uses two dialects: Steam's own files escape backslashes
/// (`"D:\\Lib"`), many game files write them literally (`"Key" "\"` binds
/// the backslash key). Escapes are tried first, the literal reading second.
pub fn parse(input: &str) -> Result<Block, String> {
    let input = input.trim_start_matches('\u{feff}');
    let with_escapes = tokenize(input, true).and_then(|t| parse_block(&t, &mut 0, true));
    with_escapes.or_else(|err| tokenize(input, false).and_then(|t| parse_block(&t, &mut 0, true)).map_err(|_| err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_app_manifest() {
        let doc = r#"
"AppState"
{
	"appid"		"730"
	"name"		"Counter-Strike 2"
	"installdir"		"Counter-Strike Global Offensive"
	"buildid"		"15912345"
	"InstalledDepots"
	{
		"731" { "manifest" "123" }
	}
}"#;
        let root = parse(doc).unwrap();
        let app = root["appstate"].as_block().unwrap();
        assert_eq!(get_str(app, "appid"), Some("730"));
        assert_eq!(get_str(app, "name"), Some("Counter-Strike 2"));
        assert_eq!(get_str(app, "buildid"), Some("15912345"));
        assert!(app["installeddepots"].as_block().unwrap().contains_key("731"));
    }

    #[test]
    fn handles_escapes_and_comments() {
        let doc = "// comment\n\"libraryfolders\" { \"0\" { \"path\" \"D:\\\\Steam\\\\Lib\" } }";
        let root = parse(doc).unwrap();
        let lib = root["libraryfolders"].as_block().unwrap()["0"].as_block().unwrap();
        assert_eq!(get_str(lib, "path"), Some("D:\\Steam\\Lib"));
    }

    #[test]
    fn literal_backslashes_as_written_by_games() {
        let doc = "\"KeyBindings\"\n{\n\t\"Keys\"\n\t{\n\t\t\"Slash\" { \"Key\" \"\\\" }\n\t\t\"Up\" { \"Key\" \"W\" }\n\t}\n}\n";
        let root = parse(doc).unwrap();
        let keys = root["keybindings"].as_block().unwrap()["keys"].as_block().unwrap();
        assert_eq!(get_str(keys["slash"].as_block().unwrap(), "key"), Some("\\"));
        assert_eq!(get_str(keys["up"].as_block().unwrap(), "key"), Some("W"));
    }

    #[test]
    fn rejects_broken_input() {
        assert!(parse("\"a\" {").is_err());
        assert!(parse("\"a\"").is_err());
        assert!(parse("}").is_err());
        assert!(parse("\"a\" \"unterminated").is_err());
    }
}
