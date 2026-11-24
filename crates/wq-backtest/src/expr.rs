//! Alpha expression parser.
//!
//! Grammar (precedence low → high): `?:` ternary, `||`, `&&`, comparisons, `+ -`, `* /`,
//! unary `- !`, `^`, atoms (numbers, fields, `name(args)` calls with optional `key=value` args,
//! parenthesised expressions). Identifiers are case-insensitive.

use std::fmt;

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum ParseError {
    #[error("unexpected character {0:?} at {1}")]
    BadChar(char, usize),
    #[error("unexpected token {0:?} at {1}")]
    BadToken(String, usize),
    #[error("unexpected end of expression")]
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    Ident(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call {
        name: String,
        args: Vec<Expr>,
        kwargs: Vec<(String, Expr)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(String),
    LParen,
    RParen,
    Comma,
}

fn tokenize(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c.is_ascii_digit()
            || (c == '.'
                && chars
                    .get(i + 1)
                    .map(|d| d.is_ascii_digit())
                    .unwrap_or(false))
        {
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || chars[i] == '.'
                    || chars[i] == 'e'
                    || chars[i] == 'E'
                    || ((chars[i] == '-' || chars[i] == '+') && matches!(chars[i - 1], 'e' | 'E')))
            {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            let v: f64 = s
                .parse()
                .map_err(|_| ParseError::BadToken(s.clone(), start))?;
            out.push((Tok::Num(v), start));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push((
                Tok::Ident(
                    chars[start..i]
                        .iter()
                        .collect::<String>()
                        .to_ascii_lowercase(),
                ),
                start,
            ));
            continue;
        }
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        if ["<=", ">=", "==", "!=", "&&", "||"].contains(&two.as_str()) {
            out.push((Tok::Op(two), start));
            i += 2;
            continue;
        }
        match c {
            '(' => out.push((Tok::LParen, start)),
            ')' => out.push((Tok::RParen, start)),
            ',' => out.push((Tok::Comma, start)),
            '+' | '-' | '*' | '/' | '^' | '<' | '>' | '!' | '?' | ':' | '=' => {
                out.push((Tok::Op(c.to_string()), start))
            }
            _ => return Err(ParseError::BadChar(c, start)),
        }
        i += 1;
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }
    fn peek_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if o == op)
    }
    fn next(&mut self) -> Result<Tok, ParseError> {
        let t = self
            .toks
            .get(self.pos)
            .map(|(t, _)| t.clone())
            .ok_or(ParseError::Eof)?;
        self.pos += 1;
        Ok(t)
    }
    fn expect(&mut self, want: &Tok) -> Result<(), ParseError> {
        let at = self
            .toks
            .get(self.pos)
            .map(|(_, p)| *p)
            .unwrap_or(usize::MAX);
        let t = self.next()?;
        if &t == want {
            Ok(())
        } else {
            Err(ParseError::BadToken(format!("{t:?}"), at))
        }
    }

    fn ternary(&mut self) -> Result<Expr, ParseError> {
        let cond = self.or()?;
        if self.peek_op("?") {
            self.pos += 1;
            let a = self.ternary()?;
            if !self.peek_op(":") {
                return Err(ParseError::BadToken("expected ':'".into(), self.pos));
            }
            self.pos += 1;
            let b = self.ternary()?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(a), Box::new(b)));
        }
        Ok(cond)
    }

    fn or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.and()?;
        while self.peek_op("||") {
            self.pos += 1;
            let rhs = self.and()?;
            lhs = Expr::Binary(BinOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.cmp()?;
        while self.peek_op("&&") {
            self.pos += 1;
            let rhs = self.cmp()?;
            lhs = Expr::Binary(BinOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn cmp(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.add()?;
        let op = match self.peek() {
            Some(Tok::Op(o)) => match o.as_str() {
                "<" => BinOp::Lt,
                "<=" => BinOp::Le,
                ">" => BinOp::Gt,
                ">=" => BinOp::Ge,
                "==" => BinOp::Eq,
                "!=" => BinOp::Ne,
                _ => return Ok(lhs),
            },
            _ => return Ok(lhs),
        };
        self.pos += 1;
        let rhs = self.add()?;
        Ok(Expr::Binary(op, Box::new(lhs), Box::new(rhs)))
    }

    fn add(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.mul()?;
        loop {
            let op = if self.peek_op("+") {
                BinOp::Add
            } else if self.peek_op("-") {
                BinOp::Sub
            } else {
                return Ok(lhs);
            };
            self.pos += 1;
            let rhs = self.mul()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn mul(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.unary()?;
        loop {
            let op = if self.peek_op("*") {
                BinOp::Mul
            } else if self.peek_op("/") {
                BinOp::Div
            } else {
                return Ok(lhs);
            };
            self.pos += 1;
            let rhs = self.unary()?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.peek_op("-") {
            self.pos += 1;
            return Ok(Expr::Unary(UnOp::Neg, Box::new(self.unary()?)));
        }
        if self.peek_op("!") {
            self.pos += 1;
            return Ok(Expr::Unary(UnOp::Not, Box::new(self.unary()?)));
        }
        self.pow()
    }

    fn pow(&mut self) -> Result<Expr, ParseError> {
        let base = self.atom()?;
        if self.peek_op("^") {
            self.pos += 1;
            let exp = self.unary()?;
            return Ok(Expr::Binary(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<Expr, ParseError> {
        let at = self
            .toks
            .get(self.pos)
            .map(|(_, p)| *p)
            .unwrap_or(usize::MAX);
        match self.next()? {
            Tok::Num(v) => Ok(Expr::Num(v)),
            Tok::LParen => {
                let e = self.ternary()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(name) => {
                if self.peek() == Some(&Tok::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    let mut kwargs = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            // keyword argument: ident '=' expr
                            if let (Some(Tok::Ident(k)), Some((Tok::Op(eq), _))) =
                                (self.peek().cloned(), self.toks.get(self.pos + 1))
                            {
                                if eq == "=" {
                                    self.pos += 2;
                                    let v = self.ternary()?;
                                    kwargs.push((k, v));
                                    if self.peek() == Some(&Tok::Comma) {
                                        self.pos += 1;
                                        continue;
                                    }
                                    break;
                                }
                            }
                            args.push(self.ternary()?);
                            if self.peek() == Some(&Tok::Comma) {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::RParen)?;
                    Ok(Expr::Call { name, args, kwargs })
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            t => Err(ParseError::BadToken(format!("{t:?}"), at)),
        }
    }
}

pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.ternary()?;
    if p.pos != p.toks.len() {
        let (t, at) = &p.toks[p.pos];
        return Err(ParseError::BadToken(format!("{t:?}"), *at));
    }
    Ok(e)
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Num(v) => write!(f, "{v}"),
            Expr::Ident(s) => write!(f, "{s}"),
            Expr::Unary(UnOp::Neg, e) => write!(f, "-({e})"),
            Expr::Unary(UnOp::Not, e) => write!(f, "!({e})"),
            Expr::Binary(op, a, b) => {
                let s = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Pow => "^",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::And => "&&",
                    BinOp::Or => "||",
                };
                write!(f, "({a} {s} {b})")
            }
            Expr::Ternary(c, a, b) => write!(f, "({c} ? {a} : {b})"),
            Expr::Call { name, args, kwargs } => {
                write!(f, "{name}(")?;
                let mut first = true;
                for a in args {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{a}")?;
                }
                for (k, v) in kwargs {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{k}={v}")?;
                }
                write!(f, ")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_and_calls() {
        let e = parse("rank(-ts_delta(Close, 5)) * 2 + volume / adv(20) ^ 2").unwrap();
        assert_eq!(
            e.to_string(),
            "((rank(-(ts_delta(close, 5))) * 2) + (volume / (adv(20) ^ 2)))"
        );
    }

    #[test]
    fn kwargs_ternary_and_comparisons() {
        let e = parse("scale(x, scale=2) > 0.5 ? a : -b").unwrap();
        assert_eq!(e.to_string(), "((scale(x, scale=2) > 0.5) ? a : -(b))");
        let e = parse("if_else(a < b && !c, 1, 2e-3)").unwrap();
        assert_eq!(e.to_string(), "if_else(((a < b) && !(c)), 1, 0.002)");
    }

    #[test]
    fn errors() {
        assert!(matches!(parse("rank(close"), Err(ParseError::Eof)));
        assert!(matches!(
            parse("close $ 2"),
            Err(ParseError::BadChar('$', 6))
        ));
        assert!(matches!(parse("close 2"), Err(ParseError::BadToken(_, 6))));
    }
}
