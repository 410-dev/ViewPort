//! Recursive-descent parser for the query language.
//!
//! ```text
//! query   := or
//! or      := and ( OR and )*
//! and     := unary ( AND unary )*
//! unary   := NOT unary | '(' or ')' | call
//! call    := NAME '(' [ param ( ':' param )* ] ')'
//! param   := raw text | "quoted \"text\""
//! ```

use crate::ast::{Call, Expr, ExprKind, Param, Span};
use crate::error::{ErrorKind, QueryError};
use crate::lang::{self, Keyword};

type Result<T> = std::result::Result<T, QueryError>;

pub fn parse(src: &str) -> Result<Expr> {
    let mut p = Parser { src, pos: 0 };
    let expr = p.parse_or()?;
    p.skip_ws();
    if let Some(ch) = p.peek() {
        let kind = if ch == ')' { ErrorKind::UnexpectedChar { ch } } else { ErrorKind::ExpectedConnective };
        let end = p.word_end().max(p.pos + ch.len_utf8());
        return Err(QueryError::new(kind, Span::new(p.pos, end)));
    }
    Ok(expr)
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

fn is_delim(c: char) -> bool {
    matches!(c, '(' | ')' | ':' | '"')
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.pos += c.len_utf8();
        }
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    /// End of the word starting at `pos` (a run of non-whitespace, non-delimiter chars).
    fn word_end(&self) -> usize {
        let len: usize = self
            .rest()
            .chars()
            .take_while(|c| !c.is_whitespace() && !is_delim(*c))
            .map(char::len_utf8)
            .sum();
        self.pos + len
    }

    fn peek_keyword(&mut self) -> Option<(Keyword, usize)> {
        self.skip_ws();
        let end = self.word_end();
        lang::keyword(&self.src[self.pos..end]).map(|k| (k, end))
    }

    fn eat_keyword(&mut self, want: Keyword) -> bool {
        match self.peek_keyword() {
            Some((k, end)) if k == want => {
                self.pos = end;
                true
            }
            _ => false,
        }
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut items = vec![self.parse_and()?];
        while self.eat_keyword(Keyword::Or) {
            items.push(self.parse_and()?);
        }
        Ok(fold(items, |items| ExprKind::Or { items }))
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut items = vec![self.parse_unary()?];
        while self.eat_keyword(Keyword::And) {
            items.push(self.parse_unary()?);
        }
        Ok(fold(items, |items| ExprKind::And { items }))
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        self.skip_ws();
        let start = self.pos;
        match self.peek() {
            None => Err(QueryError::new(ErrorKind::ExpectedCondition, Span::new(start, start))),
            Some('(') => {
                self.bump();
                let inner = self.parse_or()?;
                self.skip_ws();
                if self.peek() != Some(')') {
                    return Err(QueryError::new(ErrorKind::UnclosedParen, Span::new(start, start + 1)));
                }
                self.bump();
                Ok(Expr { span: Span::new(start, self.pos), ..inner })
            }
            Some(ch @ (')' | ':' | '"')) => {
                Err(QueryError::new(ErrorKind::UnexpectedChar { ch }, Span::new(start, start + 1)))
            }
            Some(_) => {
                if let Some((Keyword::Not, end)) = self.peek_keyword() {
                    self.pos = end;
                    let inner = self.parse_unary()?;
                    let span = Span::new(start, inner.span.end);
                    return Ok(Expr { kind: ExprKind::Not { inner: Box::new(inner) }, span });
                }
                if let Some((_, end)) = self.peek_keyword() {
                    return Err(QueryError::new(ErrorKind::ExpectedCondition, Span::new(start, end)));
                }
                self.parse_call()
            }
        }
    }

    fn parse_call(&mut self) -> Result<Expr> {
        let start = self.pos;
        // Scan the name word by word so a missing '(' before a keyword is reported
        // at the condition, not as one long bogus name.
        let mut name_end = start;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('(') => break,
                Some(_) if self.pos > start && self.peek_keyword().is_some() => {
                    return Err(missing_paren(self.src, start, name_end));
                }
                Some(c) if is_delim(c) => {
                    return Err(missing_paren(self.src, start, name_end.max(start + c.len_utf8())));
                }
                Some(_) => {
                    self.pos = self.word_end();
                    name_end = self.pos;
                }
                None => return Err(missing_paren(self.src, start, name_end)),
            }
        }
        let name_span = Span::new(start, name_end);
        let name = collapse_ws(&self.src[start..name_end]);
        let open = self.pos;
        self.bump(); // '('

        let mut params = Vec::new();
        self.skip_ws();
        if self.peek() == Some(')') {
            self.bump();
        } else {
            loop {
                params.push(self.parse_param(open)?);
                self.skip_ws();
                match self.peek() {
                    Some(':') => self.bump(),
                    Some(')') => {
                        self.bump();
                        break;
                    }
                    None => return Err(QueryError::new(ErrorKind::UnclosedParen, Span::new(open, open + 1))),
                    Some(ch) => {
                        return Err(QueryError::new(
                            ErrorKind::UnexpectedChar { ch },
                            Span::new(self.pos, self.pos + ch.len_utf8()),
                        ));
                    }
                }
            }
        }
        let span = Span::new(start, self.pos);
        Ok(Expr { kind: ExprKind::Call(Call { name, name_span, params, span }), span })
    }

    fn parse_param(&mut self, open: usize) -> Result<Param> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() == Some('"') {
            self.bump();
            let mut text = String::new();
            loop {
                match self.peek() {
                    None => {
                        return Err(QueryError::new(ErrorKind::UnterminatedString, Span::new(start, self.pos)));
                    }
                    Some('"') => {
                        self.bump();
                        break;
                    }
                    Some('\\') => {
                        self.bump();
                        match self.peek() {
                            Some(c) => {
                                text.push(c);
                                self.bump();
                            }
                            None => {
                                return Err(QueryError::new(
                                    ErrorKind::UnterminatedString,
                                    Span::new(start, self.pos),
                                ));
                            }
                        }
                    }
                    Some(c) => {
                        text.push(c);
                        self.bump();
                    }
                }
            }
            return Ok(Param { text, quoted: true, span: Span::new(start, self.pos) });
        }

        while let Some(c) = self.peek() {
            match c {
                ':' | ')' => break,
                '(' | '"' => {
                    return Err(QueryError::new(
                        ErrorKind::UnexpectedChar { ch: c },
                        Span::new(self.pos, self.pos + 1),
                    ));
                }
                _ => self.bump(),
            }
        }
        if self.peek().is_none() {
            return Err(QueryError::new(ErrorKind::UnclosedParen, Span::new(open, open + 1)));
        }
        let raw = &self.src[start..self.pos];
        let text = raw.trim_end();
        if text.is_empty() {
            return Err(QueryError::new(ErrorKind::EmptyParam, Span::new(start, self.pos)));
        }
        Ok(Param { text: text.to_string(), quoted: false, span: Span::new(start, start + text.len()) })
    }
}

fn missing_paren(src: &str, start: usize, end: usize) -> QueryError {
    let end = end.max(start);
    QueryError::new(ErrorKind::ExpectedOpenParen { name: collapse_ws(&src[start..end]) }, Span::new(start, end))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fold(mut items: Vec<Expr>, make: impl FnOnce(Vec<Expr>) -> ExprKind) -> Expr {
    if items.len() == 1 {
        return items.pop().unwrap();
    }
    let span = items.first().unwrap().span.join(items.last().unwrap().span);
    Expr { kind: make(items), span }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(e: &Expr) -> &Call {
        match &e.kind {
            ExprKind::Call(c) => c,
            other => panic!("expected call, got {other:?}"),
        }
    }

    fn err(src: &str) -> ErrorKind {
        parse(src).unwrap_err().kind
    }

    #[test]
    fn spec_example() {
        let src = "생성 날짜(2026-10-18:이후) 그리고 (변경 날짜(2026-09-18:이후) 혹은 점수(5:이상)) \
                   그리고 장르(다음을 포함:느와르:단편:코미디) 그리고 유형(다음을 포함:이미지:비디오)";
        let e = parse(src).unwrap();
        let ExprKind::And { items } = &e.kind else { panic!("{e:?}") };
        assert_eq!(items.len(), 4);
        let c = call(&items[0]);
        assert_eq!(c.name, "생성 날짜");
        assert_eq!(c.params.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["2026-10-18", "이후"]);
        assert_eq!(&src[c.name_span.start..c.name_span.end], "생성 날짜");
        let ExprKind::Or { items: ors } = &items[1].kind else { panic!() };
        assert_eq!(call(&ors[1]).name, "점수");
        assert_eq!(call(&items[2]).params.len(), 4);
    }

    #[test]
    fn and_binds_tighter_than_or() {
        let e = parse("a(1) OR b(2) AND c(3)").unwrap();
        let ExprKind::Or { items } = &e.kind else { panic!() };
        assert!(matches!(items[1].kind, ExprKind::And { .. }));
    }

    #[test]
    fn not_and_quotes() {
        let e = parse(r#"NOT 이름("a:b \"c\"")"#).unwrap();
        let ExprKind::Not { inner } = &e.kind else { panic!() };
        let p = &call(inner).params[0];
        assert_eq!(p.text, "a:b \"c\"");
        assert!(p.quoted);
    }

    #[test]
    fn params_keep_inner_spaces() {
        let e = parse("이름(  my holiday photo  )").unwrap();
        assert_eq!(call(&e).params[0].text, "my holiday photo");
    }

    #[test]
    fn empty_params() {
        assert!(call(&parse("태그()").unwrap()).params.is_empty());
    }

    #[test]
    fn errors() {
        assert_eq!(err(""), ErrorKind::ExpectedCondition);
        assert_eq!(err("이름(a) 점수(1)"), ErrorKind::ExpectedConnective);
        assert_eq!(err("이름 그리고 점수(1)"), ErrorKind::ExpectedOpenParen { name: "이름".into() });
        assert_eq!(err("이름(a"), ErrorKind::UnclosedParen);
        assert_eq!(err("(이름(a)"), ErrorKind::UnclosedParen);
        assert_eq!(err("이름(a::b)"), ErrorKind::EmptyParam);
        assert_eq!(err("이름(\"abc)"), ErrorKind::UnterminatedString);
        assert_eq!(err("이름(a(b))"), ErrorKind::UnexpectedChar { ch: '(' });
        assert_eq!(err("이름(a) 그리고"), ErrorKind::ExpectedCondition);
        assert_eq!(err("이름(a))"), ErrorKind::UnexpectedChar { ch: ')' });
    }

    #[test]
    fn error_span_points_at_problem() {
        let src = "이름(a) 그리고 점수";
        let e = parse(src).unwrap_err();
        assert_eq!(&src[e.span.start..e.span.end], "점수");
    }
}
