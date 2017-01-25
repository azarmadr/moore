// Copyright (c) 2016-2017 Fabian Schuiki

//! A parser for the SystemVerilog language. Based on IEEE 1800-2009.

use std;
use std::cell::RefCell;
use svlog::lexer::{Lexer, TokenAndSpan};
use svlog::token::*;
use std::collections::VecDeque;
use errors::*;
use svlog::ast::*;
use name::*;
use source::*;

// The problem with data_declaration and data_type_or_implicit:
//
//     [7:0] foo;            # implicit "[7:0]", var "foo"
//     foo bar;              # explicit "foo", var "bar"
//     foo [7:0];            # implicit, var "foo[7:0]"
//     foo [7:0] bar [7:0];  # explicit "foo[7:0]", var "bar[7:0]"


/// Return type of the lower parse primitives, allowing for further adjustment
/// of the diagnostic message that would be generated.
type ParseResult<T> = Result<T, DiagBuilder2>;

/// Return type of functions that emit diagnostic messages and only need to
/// communicate success to the parent.
type ReportedResult<T> = Result<T, ()>;


trait AbstractParser {
	fn peek(&mut self, offset: usize) -> TokenAndSpan;
	fn bump(&mut self);
	fn skip(&mut self);
	fn consumed(&self) -> usize;
	fn last_span(&self) -> Span;
	fn add_diag(&mut self, diag: DiagBuilder2);
	fn severity(&self) -> Severity;

	fn try_eat_ident(&mut self) -> Option<(Name, Span)> {
		match self.peek(0) {
			(Ident(name), span) => { self.bump(); Some((name, span)) },
			(EscIdent(name), span) => { self.bump(); Some((name, span)) },
			_ => None,
		}
	}

	fn eat_ident_or(&mut self, msg: &str) -> ParseResult<(Name, Span)> {
		match self.peek(0) {
			(Ident(name), span) => { self.bump(); Ok((name, span)) },
			(EscIdent(name), span) => { self.bump(); Ok((name, span)) },
			(tkn, span) => Err(DiagBuilder2::error(format!("Expected {} before `{}`", msg, tkn)).span(span)),
		}
	}

	fn eat_ident(&mut self, msg: &str) -> ReportedResult<(Name, Span)> {
		match self.peek(0) {
			(Ident(name), span) => { self.bump(); Ok((name, span)) }
			(EscIdent(name), span) => { self.bump(); Ok((name, span)) }
			(tkn, span) => {
				self.add_diag(DiagBuilder2::error(format!("Expected {} before `{}`", msg, tkn)).span(span));
				Err(())
			}
		}
	}

	fn is_ident(&mut self) -> bool {
		match self.peek(0).0 {
			Ident(_) | EscIdent(_) => true,
			_ => false,
		}
	}

	fn require(&mut self, expect: Token) -> Result<(), DiagBuilder2> {
		match self.peek(0) {
			(actual, _) if actual == expect => { self.bump(); Ok(()) },
			(wrong, span) => Err(DiagBuilder2::error(format!("Expected `{}`, but found `{}` instead", expect, wrong)).span(span))
		}
	}

	fn require_reported(&mut self, expect: Token) -> ReportedResult<()> {
		match self.require(expect) {
			Ok(x) => Ok(x),
			Err(e) => {
				self.add_diag(e);
				Err(())
			}
		}
	}

	fn try_eat(&mut self, expect: Token) -> bool {
		match self.peek(0) {
			(actual, _) if actual == expect => { self.bump(); true },
			_ => false
		}
	}

	fn recover(&mut self, terminators: &[Token], eat_terminator: bool) {
		// println!("recovering to {:?}", terminators);
		loop {
			match self.peek(0) {
				(Eof, _) => return,
				(tkn, _) => {
					for t in terminators {
						if *t == tkn {
							if eat_terminator {
								self.skip();
							}
							return;
						}
					}
					self.skip();
				}
			}
		}
	}

	fn recover_balanced(&mut self, terminators: &[Token], eat_terminator: bool) {
		// println!("recovering (balanced) to {:?}", terminators);
		let mut stack = Vec::new();
		loop {
			let (tkn, sp) = self.peek(0);
			if stack.is_empty() {
				for t in terminators {
					if *t == tkn {
						if eat_terminator {
							self.skip();
						}
						return;
					}
				}
			}

			match tkn {
				OpenDelim(x) => stack.push(x),
				CloseDelim(x) => {
					if let Some(open) = stack.pop() {
						if open != x {
							self.add_diag(DiagBuilder2::error(format!("Found closing `{}` which is not the complement to the previous opening `{}`", CloseDelim(x), OpenDelim(open))).span(sp));
							break;
						}
					} else {
						self.add_diag(DiagBuilder2::error(format!("Found closing `{}` without an earlier opening `{}`", CloseDelim(x), OpenDelim(x))).span(sp));
						break;
					}
				}
				Eof => break,
				_ => (),
			}
			self.skip();
		}
	}

	fn is_fatal(&self) -> bool {
		self.severity() >= Severity::Fatal
	}

	fn is_error(&self) -> bool {
		self.severity() >= Severity::Error
	}

	fn anticipate(&mut self, tokens: &[Token]) -> ReportedResult<()> {
		let (tkn,sp) = self.peek(0);
		for t in tokens {
			if *t == tkn {
				return Ok(());
			}
		}
		self.add_diag(DiagBuilder2::error(format!("Expected {:?}, but found {:?} instead", tokens, tkn)).span(sp));
		Err(())
	}
}

struct Parser<'a> {
	input: Lexer<'a>,
	queue: VecDeque<TokenAndSpan>,
	diagnostics: Vec<DiagBuilder2>,
	last_span: Span,
	severity: Severity,
	consumed: usize,
}

impl<'a> AbstractParser for Parser<'a> {
	fn peek(&mut self, offset: usize) -> TokenAndSpan {
		self.ensure_queue_filled(offset);
		if offset < self.queue.len() {
			self.queue[offset]
		} else {
			*self.queue.back().expect("At least an Eof token should be in the queue")
		}
	}

	fn bump(&mut self) {
		if self.queue.is_empty() {
			self.ensure_queue_filled(1);
		}
		if let Some((_,sp)) = self.queue.pop_front() {
			self.last_span = sp;
			self.consumed += 1;
		}
	}

	fn skip(&mut self) {
		self.bump()
	}

	fn consumed(&self) -> usize {
		self.consumed
	}

	fn last_span(&self) -> Span {
		self.last_span
	}

	fn add_diag(&mut self, diag: DiagBuilder2) {
		println!("");
		println!("{}", diag);

		// Keep track of the worst diagnostic severity we've encountered, such
		// that parsing can be aborted accordingly.
		if diag.get_severity() > self.severity {
			self.severity = diag.get_severity();
		}
		self.diagnostics.push(diag);
	}

	fn severity(&self) -> Severity {
		self.severity
	}
}

impl<'a> Parser<'a> {
	fn new(input: Lexer) -> Parser {
		Parser {
			input: input,
			queue: VecDeque::new(),
			diagnostics: Vec::new(),
			last_span: INVALID_SPAN,
			severity: Severity::Note,
			consumed: 0,
		}
	}

	fn ensure_queue_filled(&mut self, min_tokens: usize) {
		if let Some(&(Eof,_)) = self.queue.back() {
			return;
		}
		while self.queue.len() <= min_tokens {
			match self.input.next_token() {
				Ok((Eof, sp)) => self.queue.push_back((Eof, sp)),
				Ok(tkn) => self.queue.push_back(tkn),
				Err(x) => self.add_diag(x),
			}
		}
	}

	fn get_diagnostics(&self) -> &[DiagBuilder2] {
		&self.diagnostics
	}
}


fn parenthesized<R,F>(p: &mut AbstractParser, mut inner: F) -> ReportedResult<R>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	flanked(p, Paren, inner)
}

/// Parses the opening delimiter, calls the `inner` function, and parses the
/// closing delimiter. Properly recovers to and including the closing
/// delimiter if the `inner` function throws an error.
fn flanked<R,F>(p: &mut AbstractParser, delim: DelimToken, mut inner: F) -> ReportedResult<R>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	p.require_reported(OpenDelim(delim))?;
	match inner(p) {
		Ok(r) => {
			p.require_reported(CloseDelim(delim))?;
			Ok(r)
		}
		Err(e) => {
			p.recover_balanced(&[CloseDelim(delim)], true);
			Err(e)
		}
	}
}

/// If the opening delimiter is present, consumes it, calls the `inner`
/// function, and parses the closing delimiter. Properly recovers to and
/// including the closing delimiter if the `inner` function throws an error.
/// If the opening delimiter is not present, returns `None`.
fn try_flanked<R,F>(p: &mut AbstractParser, delim: DelimToken, mut inner: F) -> ReportedResult<Option<R>>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	if p.peek(0).0 == OpenDelim(delim) {
		flanked(p, delim, inner).map(|r| Some(r))
	} else {
		Ok(None)
	}
}

/// Parse a comma-separated list of items, until a terminator token has been
/// reached. The terminator is not consumed.
fn comma_list<R,F>(p: &mut AbstractParser, term: Token, msg: &str, mut item: F) -> ReportedResult<Vec<R>>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	let mut v = Vec::new();
	while !p.is_fatal() && p.peek(0).0 != term && p.peek(0).0 != Eof {
		// Parse the item.
		match item(p) {
			Ok(x) => v.push(x),
			Err(e) => {
				p.recover_balanced(&[term], false);
				return Err(e);
			},
		}

		// Consume a comma or the terminator.
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == term {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			}
			(x, _) if x == term => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or {} after {}", term, msg)).span(sp));
				p.recover_balanced(&[term], false);
				return Err(());
			}
		}
	}
	Ok(v)
}


/// Same as `comma_list`, but at least one item is required.
fn comma_list_nonempty<R,F>(p: &mut AbstractParser, term: Token, msg: &str, mut item: F) -> ReportedResult<Vec<R>>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	let q = p.peek(0).1;
	let v = comma_list(p, term, msg, item)?;
	if v.is_empty() {
		p.add_diag(DiagBuilder2::error(format!("Expected at least one {}", msg)).span(q));
		Err(())
	} else {
		Ok(v)
	}
}

fn repeat_until<R,F>(p: &mut AbstractParser, term: Token, mut item: F) -> ReportedResult<Vec<R>>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	let mut v = Vec::new();
	while p.peek(0).0 != term && p.peek(0).0 != Eof {
		match item(p) {
			Ok(x) => v.push(x),
			Err(e) => {
				p.recover_balanced(&[term], false);
				break;
			}
		}
	}
	Ok(v)
}

fn recovered<R,F>(p: &mut AbstractParser, term: Token, mut item: F) -> ReportedResult<R>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	match item(p) {
		Ok(x) => Ok(x),
		Err(e) => {
			p.recover_balanced(&[term], false);
			Err(e)
		}
	}
}

/// Speculatively apply a parse function. If it fails, the parser `p` is left
/// untouched. If it succeeds, `p` is in the same state as if `parse` was called
/// on it directly. Use a ParallelParser for better error reporting.
fn try<R,F>(p: &mut AbstractParser, mut parse: F) -> Option<R>
where F: FnMut(&mut AbstractParser) -> ReportedResult<R> {
	let mut bp = BranchParser::new(p);
	match parse(&mut bp) {
		Ok(r) => {
			bp.commit();
			Some(r)
		}
		Err(_) => None
	}
}


pub fn parse(input: Lexer) -> Result<Root, ()> {
	let mut p = Parser::new(input);
	let root = parse_source_text(&mut p);
	if p.is_error() {
		Err(())
	} else {
		Ok(root)
	}
}

fn parse_source_text(p: &mut Parser) -> Root {
	let mut root = Root {
		timeunits: None,
		items: Vec::new(),
	};

	// Parse the optional timeunits declaration.
	// TODO

	// Parse the descriptions in the source text.
	while p.peek(0).0 != Eof {
		match parse_item(p) {
			Ok(item) => root.items.push(item),
			Err(()) => {
				p.recover_balanced(&[
					Keyword(Kw::Endmodule),
					Keyword(Kw::Endinterface),
					Keyword(Kw::Endpackage),
					Keyword(Kw::Endprogram)
				], true);
			}
		}
	}

	root
}


fn parse_item(p: &mut Parser) -> ReportedResult<Item> {
	let (tkn,sp) = p.peek(0);
	match tkn {
		Keyword(Kw::Module) => parse_module_decl(p).map(|d| Item::Module(d)),
		Keyword(Kw::Interface) => parse_interface_decl(p).map(|d| Item::Interface(d)),
		Keyword(Kw::Package) => parse_package_decl(p).map(|d| Item::Package(d)),
		// Keyword(Kw::Program) => parse_program_decl(p).map(|d| Item::Module(d)),
		Keyword(Kw::Import) => parse_import_decl(p).map(|i| Item::Item(HierarchyItem::ImportDecl(i))),
		tkn => {
			p.add_diag(DiagBuilder2::fatal(format!("Expected module, interface, package, program, or import, instead got `{}`", tkn)).span(sp));
			Err(())
		}
	}
}



/// Convert a token to the corresponding lifetime. Yields `None` if the token
/// does not correspond to a lifetime.
fn as_lifetime(tkn: Token) -> Option<Lifetime> {
	match tkn {
		Keyword(Kw::Static) => Some(Lifetime::Static),
		Keyword(Kw::Automatic) => Some(Lifetime::Automatic),
		_ => None,
	}
}


fn parse_interface_decl(p: &mut Parser) -> ReportedResult<IntfDecl> {
	let mut span = p.peek(0).1;
	p.require_reported(Keyword(Kw::Interface));

	// Eat the optional lifetime.
	let lifetime = match as_lifetime(p.peek(0).0) {
		Some(l) => { p.bump(); l },
		None => Lifetime::Static,
	};

	// Eat the interface name.
	let (name, name_sp) = p.eat_ident("interface name")?;

	// TODO: Parse package import declarations.

	// Eat the parameter port list.
	let param_ports = if p.try_eat(Hashtag) {
		parse_parameter_port_list(p)?
	} else {
		Vec::new()
	};

	// Eat the optional list of ports.
	let ports = if p.try_eat(OpenDelim(Paren)) {
		parse_port_list(p)?
	} else {
		Vec::new()
	};

	// Eat the semicolon at the end of the header.
	if !p.try_eat(Semicolon) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing semicolon \";\" after header of interface \"{}\"", name)).span(q));
	}

	// Eat the items in the interface.
	while p.peek(0).0 != Keyword(Kw::Endinterface) && p.peek(0).0 != Eof {
		// Skip empty items.
		if p.try_eat(Semicolon) {
			continue;
		}

		let q = p.peek(0).1;
		match parse_hierarchy_item(p) {
			Ok(_) => (),
			Err(()) => {
				// p.add_diag(DiagBuilder2::error("Expected hierarchy item").span(q));
				p.recover(&[Keyword(Kw::Endinterface)], false);
				break;
			}
		}
	}

	// Eat the endinterface keyword.
	if !p.try_eat(Keyword(Kw::Endinterface)) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing \"endinterface\" at the end of \"{}\"", name)).span(q));
	}

	span.expand(p.last_span());
	Ok(IntfDecl {
		span: span,
		lifetime: lifetime,
		name: name,
		name_span: name_sp,
		ports: ports,
	})
}


fn parse_parameter_port_list(p: &mut AbstractParser) -> ReportedResult<Vec<()>> {
	let mut v = Vec::new();
	p.require_reported(OpenDelim(Paren))?;

	while p.try_eat(Keyword(Kw::Parameter)) {
		// TODO: Parse data type or implicit type.

		// Eat the list of parameter assignments.
		loop {
			// parameter_identifier { unpacked_dimension } [ = constant_param_expression ]
			let (name, name_sp) = match p.eat_ident("parameter name") {
				Ok(x) => x,
				Err(()) => {
					p.recover_balanced(&[Comma, CloseDelim(Paren)], false);
					break;
				}
			};

			// TODO: Eat the unpacked dimensions.

			if p.try_eat(Operator(Op::Assign)) {
				match parse_constant_expr(p) {
					Ok(_) => (),
					Err(_) => p.recover_balanced(&[Comma, CloseDelim(Paren)], false)
				}
			}

			v.push(());

			// Eat the trailing comma or closing parenthesis.
			match p.peek(0) {
				(Comma, sp) => {
					p.bump();
					match p.peek(0) {
						// The `parameter` keyword terminates this list of
						// assignments and introduces the next parameter.
						(Keyword(Kw::Parameter), _) => break,

						// A closing parenthesis indicates that the previous
						// comma was superfluous. Report the issue but continue
						// gracefully.
						(CloseDelim(Paren), _) => {
							// TODO: This should be an error in pedantic mode.
							p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
							break;
						}

						// All other tokens indicate the next assignment in the
						// list, so we just continue with the next iteration.
						_ => continue,
					}
				},
				(CloseDelim(Paren), _) => break,
				(_, sp) => {
					p.add_diag(DiagBuilder2::error("Expected , or ) after parameter assignment").span(sp));
					p.recover_balanced(&[CloseDelim(Paren)], false);
					break;
				}
			}
		}
	}

	p.require_reported(CloseDelim(Paren))?;
	Ok(v)
}


fn parse_constant_expr(p: &mut AbstractParser) -> ReportedResult<()> {
	parse_expr(p)?;
	Ok(())
	// let (tkn, span) = p.peek(0);

	// // Try the unary operators.
	// if let Some(x) = as_unary_operator(tkn) {
	// 	p.bump();
	// 	return parse_constant_expr(p);
	// }

	// // Parse the constant primary.
	// let expr = match tkn {
	// 	// Primary literals.
	// 	UnsignedNumber(x) => { p.bump(); () },
	// 	Literal(Str(x)) => { p.bump(); () },
	// 	Literal(BasedInteger(size, signed, base, value)) => { p.bump(); () },
	// 	Literal(UnbasedUnsized(x)) => { p.bump(); () },
	// 	Ident(x) => { p.bump(); () },
	// 	_ => {
	// 		p.add_diag(DiagBuilder2::error("Expected constant primary expression").span(span));
	// 		return Err(());
	// 	}
	// };

	// // Try the binary operators.
	// let (tkn, span) = p.peek(0);
	// if let Some(x) = as_binary_operator(tkn) {
	// 	p.bump();
	// 	return parse_constant_expr(p);
	// }

	// // TODO: Parse ternary operator.

	// Ok(())
}


/// Parse a module declaration, assuming that the leading `module` keyword has
/// already been consumed.
fn parse_module_decl(p: &mut Parser) -> ReportedResult<ModDecl> {
	let mut span = p.peek(0).1;
	p.require_reported(Keyword(Kw::Module));

	// Eat the optional lifetime.
	let lifetime = match as_lifetime(p.peek(0).0) {
		Some(l) => { p.bump(); l },
		None => Lifetime::Static,
	};

	// Eat the module name.
	let (name, name_sp) = p.eat_ident("module name")?;

	// TODO: Parse package import declarations.

	// Eat the optional parameter port list.
	let param_ports = if p.try_eat(Hashtag) {
		parse_parameter_port_list(p)?
	} else {
		Vec::new()
	};

	// Eat the optional list of ports. Not having such a list requires the ports
	// to be defined further down in the body of the module.
	let ports = if p.try_eat(OpenDelim(Paren)) {
		parse_port_list(p)?
	} else {
		Vec::new()
	};

	// Eat the semicolon after the header.
	if !p.try_eat(Semicolon) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing ; after header of module \"{}\"", name)).span(q));
	}

	// Eat the items in the module.
	while p.peek(0).0 != Keyword(Kw::Endmodule) && p.peek(0).0 != Eof {
		// Skip empty items.
		if p.try_eat(Semicolon) {
			continue;
		}

		let q = p.peek(0).1;
		match parse_hierarchy_item(p) {
			Ok(_) => (),
			Err(()) => {
				// p.add_diag(DiagBuilder2::error("Expected hierarchy item").span(q));
				p.recover(&[Keyword(Kw::Endmodule)], false);
				break;
			}
		}
	}

	// Eat the endmodule keyword.
	if !p.try_eat(Keyword(Kw::Endmodule)) {
		let q = p.peek(0).1.end();
		p.add_diag(DiagBuilder2::error(format!("Missing \"endmodule\" at the end of \"{}\"", name)).span(q));
	}

	span.expand(p.last_span());
	Ok(ModDecl {
		span: span,
		lifetime: lifetime,
		name: name,
		name_span: name_sp,
		ports: ports,
	})
}


fn parse_package_decl(p: &mut AbstractParser) -> ReportedResult<PackageDecl> {
	let mut span = p.peek(0).1;
	p.require_reported(Keyword(Kw::Package))?;
	let result = recovered(p, Keyword(Kw::Endpackage), |p|{

		// Parse the optional lifetime.
		let lifetime = match as_lifetime(p.peek(0).0) {
			Some(x) => { p.bump(); x },
			None => Lifetime::Static,
		};

		// Parse the package name.
		let (name, name_span) = p.eat_ident("package name")?;
		p.require_reported(Semicolon)?;

		// TODO: Parse the optional timeunits declaration.
		let timeunits = Timeunit;

		// Parse the package items.
		let mut items = Vec::new();
		while !p.is_fatal() && p.peek(0).0 != Keyword(Kw::Endpackage) && p.peek(0).0 != Eof {
			if p.try_eat(Semicolon) {
				continue;
			}
			items.push(parse_hierarchy_item(p)?);
		}

		span.expand(p.last_span());
		Ok(PackageDecl {
			span: span,
			lifetime: lifetime,
			name: name,
			name_span: name_span,
			timeunits: timeunits,
			items: items,
		})
	});
	p.require_reported(Keyword(Kw::Endpackage))?;
	result
}


fn parse_program_decl(p: &mut AbstractParser) -> ReportedResult<()> {
	p.require_reported(Keyword(Kw::Program))?;
	let result = recovered(p, Keyword(Kw::Endprogram), |p|{
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Don't know how to parse program declarations").span(q));
		Err(())
	});
	p.require_reported(Keyword(Kw::Endprogram))?;
	result
}


fn parse_hierarchy_item(p: &mut AbstractParser) -> ReportedResult<HierarchyItem> {
	// First attempt the simple cases where a keyword reliably identifies the
	// following item.
	match p.peek(0).0 {
		Keyword(Kw::Localparam) => return parse_localparam_decl(p).map(|x| HierarchyItem::LocalparamDecl(x)),
		Keyword(Kw::Parameter)  => return parse_parameter_decl(p).map(|x| HierarchyItem::ParameterDecl(x)),
		Keyword(Kw::Modport)    => return parse_modport_decl(p).map(|x| HierarchyItem::ModportDecl(x)),
		Keyword(Kw::Class)      => return parse_class_decl(p).map(|x| HierarchyItem::ClassDecl(x)),
		Keyword(Kw::Typedef)    => return parse_typedef(p).map(|x| HierarchyItem::Typedef(x)),
		Keyword(Kw::Import)     => return parse_import_decl(p).map(|x| HierarchyItem::ImportDecl(x)),

		// Structured procedures as per IEEE 1800-2009 section 9.2
		Keyword(Kw::Initial)     => return parse_procedure(p, ProcedureKind::Initial).map(|x| HierarchyItem::Procedure(x)),
		Keyword(Kw::Always)      => return parse_procedure(p, ProcedureKind::Always).map(|x| HierarchyItem::Procedure(x)),
		Keyword(Kw::AlwaysComb)  => return parse_procedure(p, ProcedureKind::AlwaysComb).map(|x| HierarchyItem::Procedure(x)),
		Keyword(Kw::AlwaysLatch) => return parse_procedure(p, ProcedureKind::AlwaysLatch).map(|x| HierarchyItem::Procedure(x)),
		Keyword(Kw::AlwaysFf)    => return parse_procedure(p, ProcedureKind::AlwaysFf).map(|x| HierarchyItem::Procedure(x)),
		Keyword(Kw::Final)       => return parse_procedure(p, ProcedureKind::Final).map(|x| HierarchyItem::Procedure(x)),
		Keyword(Kw::Function) | Keyword(Kw::Task) => return parse_subroutine_decl(p).map(|x| HierarchyItem::SubroutineDecl(x)),

		// Port declarations
		Keyword(Kw::Inout) |
		Keyword(Kw::Input) |
		Keyword(Kw::Output) |
		Keyword(Kw::Ref) => return parse_port_decl(p).map(|x| HierarchyItem::PortDecl(x)),

		// Continuous assign
		Keyword(Kw::Assign) => return parse_continuous_assign(p).map(|_| HierarchyItem::ContAssign),

		// Genvar declaration
		Keyword(Kw::Genvar) => {
			p.bump();
			comma_list_nonempty(p, Semicolon, "genvar declaration", parse_genvar_decl)?;
			p.require_reported(Semicolon)?;
			return Ok(HierarchyItem::GenvarDecl);
		}

		// Generate region and constructs
		Keyword(Kw::Generate) => {
			p.bump();
			repeat_until(p, Keyword(Kw::Endgenerate), parse_generate_item)?;
			p.require_reported(Keyword(Kw::Endgenerate))?;
			return Ok(HierarchyItem::GenerateRegion);
		}
		Keyword(Kw::For)  => return parse_generate_for(p).map(|_| HierarchyItem::GenerateFor),
		Keyword(Kw::If)   => return parse_generate_if(p).map(|_| HierarchyItem::GenerateIf),
		Keyword(Kw::Case) => return parse_generate_case(p).map(|_| HierarchyItem::GenerateCase),

		// Assertions
		Keyword(Kw::Assert) |
		Keyword(Kw::Assume) |
		Keyword(Kw::Cover) |
		Keyword(Kw::Expect) |
		Keyword(Kw::Restrict) => return parse_assertion(p).map(|x| HierarchyItem::Assertion(x)),

		_ => ()
	}

	// Handle net declarations.
	if as_net_type(p.peek(0).0).is_some() {
		return parse_net_decl(p).map(|x| HierarchyItem::NetDecl(x));
	}

	// TODO: Handle the const and var keywords that may appear in front of a
	// data declaration, as well as the optional lifetime.
	let konst = p.try_eat(Keyword(Kw::Const));
	let var = p.try_eat(Keyword(Kw::Var));
	let lifetime = as_lifetime(p.peek(0).0);
	if lifetime.is_some() {
		p.bump();
	}

	// Now attempt to parse a data type or implicit type, which could introduce
	// and instantiation or data declaration. Due to the nature of implicit
	// types, a data declaration such as `foo[7:0];` would initially parse as an
	// explicit type `foo[7:0]`, and can only be identified as having an
	// implicit type when the semicolon is parsed. I.e. declarations that appear
	// to consist only of a type are actually declarations with an implicit
	// type.
	let ty = match parse_data_type(p) {
		Ok(x) => x,
		Err(_) => {
			p.recover_balanced(&[Semicolon], true);
			return Err(());
		}
	};

	// TODO: Handle the special case where the token following the parsed data
	// type is a [,;=], which indicates that the parsed type is actually a
	// variable declaration with implicit type (they look the same).

	// In case this is an instantiation, some parameter assignments may follow.
	if p.try_eat(Hashtag) {
		parse_parameter_assignments(p)?;
	}

	// Parse the list of variable declaration assignments.
	loop {
		let (name, span) = p.eat_ident("variable or instance name")?;

		// Parse the optional variable dimensions.
		let dims = match parse_optional_dimensions(p) {
			Ok(x) => x,
			Err(_) => return Err(()),
		};

		// Parse the optional assignment.
		match p.peek(0) {
			(Operator(Op::Assign), sp) => {
				p.bump();
				parse_expr(p)?;
			}
			(OpenDelim(Paren), sp) => {
				flanked(p, Paren, parse_list_of_port_connections)?;
			}
			_ => ()
		}

		// Either parse the next variable declaration or break out of the loop
		// if we have encountered the semicolon that terminates the statement.
		match p.peek(0) {
			(Semicolon, _) => { p.bump(); break; },
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == Semicolon {
					// TODO: Make this an error in pedantic mode.
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					p.bump();
					break;
				} else {
					continue;
				}
			}
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ; after variable declaration").span(sp));
				p.recover(&[Semicolon], true);
				return Err(());
			}
		}
	}

	Ok(HierarchyItem::DataDecl)
}


fn parse_localparam_decl(p: &mut AbstractParser) -> ReportedResult<()> {
	p.require_reported(Keyword(Kw::Localparam))?;
	// TODO: Parse data type or implicit type.

	// Eat the list of parameter assignments.
	loop {
		// parameter_identifier { unpacked_dimension } [ = constant_param_expression ]
		let (name, name_sp) = match p.eat_ident_or("parameter name") {
			Ok(x) => x,
			Err(e) => {
				p.add_diag(e);
				return Err(());
			}
		};

		// TODO: Eat the unpacked dimensions.

		// Eat the optional assignment.
		if p.try_eat(Operator(Op::Assign)) {
			match parse_expr(p) {
				Ok(_) => (),
				Err(_) => p.recover_balanced(&[Comma, Semicolon], false)
			}
		}

		// Eat the trailing comma or semicolon.
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();

				// A closing parenthesis indicates that the previous
				// comma was superfluous. Report the issue but continue
				// gracefully.
				if p.peek(0).0 == Semicolon {
					// TODO: This should be an error in pedantic mode.
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(Semicolon, _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ; after localparam, found {}", x)).span(sp));
				return Err(());
			}
		}
	}
	p.require_reported(Semicolon)?;
	Ok(())
}


fn parse_parameter_decl(p: &mut AbstractParser) -> ReportedResult<()> {
	p.require_reported(Keyword(Kw::Parameter))?;

	// Branch to try the explicit and implicit type version.
	let mut pp = ParallelParser::new();
	pp.add("explicit type", |p|{
		let ty = parse_explicit_type(p)?;
		Ok((ty, tail(p)?))
	});
	pp.add("implicit type", |p|{
		let ty = parse_implicit_type(p)?;
		Ok((ty, tail(p)?))
	});
	let (ty, ()) = pp.finish(p, "explicit or implicit type")?;

	fn tail(p: &mut AbstractParser) -> ReportedResult<()> {
		let names = parse_parameter_names(p)?;
		p.require_reported(Semicolon)?;
		Ok(())
	}

	return Ok(())
}


fn parse_parameter_names(p: &mut AbstractParser) -> ReportedResult<Vec<()>> {
	let v = comma_list_nonempty(p, Semicolon, "parameter name", |p|{
		// Consume the parameter name and optional dimensions.
		let (name, name_sp) = p.eat_ident("parameter name")?;
		let (dims, _) = parse_optional_dimensions(p)?;

		// Parse the optional assignment.
		let expr = if p.try_eat(Operator(Op::Assign)) {
			Some(parse_expr(p)?)
		} else {
			None
		};

		Ok(())
	})?;
	Ok(v)
}


/// Parse a modport declaration.
///
/// ```
/// modport_decl: "modport" modport_item {"," modport_item} ";"
/// modport_item: ident "(" modport_ports_decl {"," modport_ports_decl} ")"
/// modport_ports_decl:
///   port_direction modport_simple_port {"," modport_simple_port} |
///   ("import"|"export") modport_tf_port {"," modport_tf_port} |
///   "clocking" ident
/// modport_simple_port: ident | "." ident "(" [expr] ")"
/// ```
fn parse_modport_decl(p: &mut AbstractParser) -> ReportedResult<()> {
	p.require_reported(Keyword(Kw::Modport))?;
	loop {
		parse_modport_item(p)?;
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if let (Semicolon, _) = p.peek(0) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				} else {
					continue;
				}
			},
			(Semicolon, _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ; after modport declaration, got `{:?}`", x)).span(sp));
				return Err(());
			}
		}
	}

	Ok(())
}


fn parse_modport_item(p: &mut AbstractParser) -> ReportedResult<()> {
	let (name, span) = match p.eat_ident_or("modport name") {
		Ok(x) => x,
		Err(e) => {
			p.add_diag(e);
			return Err(());
		}
	};

	// Eat the opening parenthesis.
	if !p.try_eat(OpenDelim(Paren)) {
		let (tkn, q) = p.peek(0);
		p.add_diag(DiagBuilder2::error(format!("Expected ( after modport name `{}`, got `{:?}`", name, tkn)).span(q));
		return Err(());
	}

	// Parse the port declarations.
	loop {
		match parse_modport_port_decl(p) {
			Ok(x) => x,
			Err(_) => {
				p.recover(&[CloseDelim(Paren)], true);
				return Err(());
			}
		}
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if let (CloseDelim(Paren), _) = p.peek(0) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				} else {
					continue;
				}
			}
			(CloseDelim(Paren), _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ) after port declaration, got `{:?}`", x)).span(sp));
				return Err(());
			}
		}
	}

	// Eat the closing parenthesis.
	if !p.try_eat(CloseDelim(Paren)) {
		let (tkn, q) = p.peek(0);
		p.add_diag(DiagBuilder2::error(format!("Expected ) after port list of modport `{}`, got `{:?}`", name, tkn)).span(q));
		return Err(());
	}

	Ok(())
}


/// ```
/// modport_ports_decl:
///   port_direction modport_simple_port {"," modport_simple_port} |
///   ("import"|"export") modport_tf_port {"," modport_tf_port} |
///   "clocking" ident
/// modport_simple_port: ident | "." ident "(" [expr] ")"
/// ```
fn parse_modport_port_decl(p: &mut AbstractParser) -> ReportedResult<()> {
	let (tkn, span) = p.peek(0);

	// Attempt to parse a simple port introduced by one of the port direction
	// keywords.
	if let Some(dir) = as_port_direction(tkn) {
		p.bump();
		loop {
			if p.try_eat(Period) {
				let (name, span) = p.eat_ident("port name")?;
				p.require_reported(OpenDelim(Paren))?;
				// TODO: Parse expression.
				p.require_reported(CloseDelim(Paren))?;
			} else {
				let (name, span) = p.eat_ident("port name")?;
			}

			// Decide whether we should continue iterating and thus consuming
			// more simple ports. According to the grammar, a comma followed by
			// a keyword indicates a different port declaration, so we abort.
			// Otherwise, if the next item is a comma still, we continue
			// iteration. In all other cases, we assume the port declaration to
			// be done.
			match (p.peek(0).0, p.peek(1).0) {
				(Comma, Keyword(_)) => break,
				(Comma, _) => {
					p.bump();
					continue;
				},
				_ => break,
			}
		}
		return Ok(());
	}

	// TODO: Parse modport_tf_port.

	// Attempt to parse a clocking declaration.
	if p.try_eat(Keyword(Kw::Clocking)) {
		// TODO: Parse modport_clocking_declaration.
		p.add_diag(DiagBuilder2::error("modport clocking declaration not implemented").span(span));
		return Err(());
	}

	// If we've come thus far, none of the above matched.
	p.add_diag(DiagBuilder2::error("Expected port declaration").span(span));
	Err(())
}

/// Convert a token to the corresponding PortDir. The token may be one of the
/// keywords `input`, `output`, `inout`, or `ref`. Otherwise `None` is returned.
fn as_port_direction(tkn: Token) -> Option<PortDir> {
	match tkn {
		Keyword(Kw::Input) => Some(PortDir::Input),
		Keyword(Kw::Output) => Some(PortDir::Output),
		Keyword(Kw::Inout) => Some(PortDir::Inout),
		Keyword(Kw::Ref) => Some(PortDir::Ref),
		_ => None,
	}
}


/// Parse an implicit or explicit type. This is a catch-all function that will
/// always succeed unless one of the explicit types contains a syntax error. For
/// all other tokens the function will at least return an ImplicitType if none
/// could be consumed. You might have to use `parse_explicit_type` and
/// `parse_implicit_type` separately if the type is to be embedded in a larger
/// function. For example, a variable declaration with implicit type looks like
/// an explicit type at first glance. Only after reaching the trailing `[=,;]`
/// it becomes apparent that the explicit type was rather the name of the
/// variable. In this case, having to parallel parsers, one with explicit and
/// one with implicit type, can resolve the issue.
fn parse_data_type(p: &mut AbstractParser) -> ReportedResult<Type> {
	// Try to parse this as an explicit type.
	{
		let mut bp = BranchParser::new(p);
		match parse_explicit_type(&mut bp) {
			Ok(x) => {
				bp.commit();
				return Ok(x);
			},
			Err(_) => ()
		}
	}

	// Otherwise simply go with an implicit type, which basically always
	// succeeds.
	parse_implicit_type(p)
}


fn parse_explicit_type(p: &mut AbstractParser) -> ReportedResult<Type> {
	let mut span = p.peek(0).1;
	let data = parse_type_data(p)?;
	span.expand(p.last_span());
	let ty = parse_type_signing_and_dimensions(p, span, data)?;
	parse_type_suffix(p, ty)
}


fn parse_type_suffix(p: &mut AbstractParser, ty: Type) -> ReportedResult<Type> {
	let (tkn, sp) = p.peek(0);
	match tkn {
		// Interfaces allow their internal modports and typedefs to be accessed
		// via the `.` operator.
		Period => {
			p.bump();
			let (name, name_span) = p.eat_ident("member type name")?;
			let subty = parse_type_signing_and_dimensions(p, sp, ScopedType {
				ty: Box::new(ty),
				member: true,
				name: name,
				name_span: name_span,
			})?;
			parse_type_suffix(p, subty)
		}

		// The `::` operator.
		Namespace => {
			p.bump();
			let (name, name_span) = p.eat_ident("type name")?;
			let subty = parse_type_signing_and_dimensions(p, sp, ScopedType {
				ty: Box::new(ty),
				member: false,
				name: name,
				name_span: name_span,
			})?;
			parse_type_suffix(p, subty)
		}

		_ => Ok(ty)
	}
}


/// Parse an implicit type (`[signing] {dimensions}`).
fn parse_implicit_type(p: &mut AbstractParser) -> ReportedResult<Type> {
	let span = p.peek(0).1.begin().into();
	parse_type_signing_and_dimensions(p, span, ImplicitType)
}


/// Parse the optional signing keyword and packed dimensions that may follow a
/// data type. Wraps a previously parsed TypeData in a Type struct.
fn parse_type_signing_and_dimensions(p: &mut AbstractParser, mut span: Span, data: TypeData) -> ReportedResult<Type> {
	// Parse the optional sign information.
	let sign = match p.peek(0).0 {
		Keyword(Kw::Signed)   => { p.bump(); TypeSign::Signed   },
		Keyword(Kw::Unsigned) => { p.bump(); TypeSign::Unsigned },
		_ => TypeSign::None
	};

	// Parse the optional dimensions.
	let (dims, _) = parse_optional_dimensions(p)?;
	span.expand(p.last_span());

	Ok(Type {
		span: span,
		data: data,
		sign: sign,
		dims: dims,
	})
}

/// Parse the core type data of a type.
fn parse_type_data(p: &mut AbstractParser) -> ReportedResult<TypeData> {
	match p.peek(0).0 {
		Keyword(Kw::Void)    => { p.bump(); Ok(VoidType) },
		Keyword(Kw::String)  => { p.bump(); Ok(StringType) },
		Keyword(Kw::Chandle) => { p.bump(); Ok(ChandleType) },
		Keyword(Kw::Event)   => { p.bump(); Ok(EventType) },

		// Integer Vector Types
		Keyword(Kw::Bit)   => { p.bump(); Ok(BitType) },
		Keyword(Kw::Logic) => { p.bump(); Ok(LogicType) },
		Keyword(Kw::Reg)   => { p.bump(); Ok(RegType) },

		// Integer Atom Types
		Keyword(Kw::Byte)     => { p.bump(); Ok(ByteType) },
		Keyword(Kw::Shortint) => { p.bump(); Ok(ShortIntType) },
		Keyword(Kw::Int)      => { p.bump(); Ok(IntType) },
		Keyword(Kw::Longint)  => { p.bump(); Ok(LongIntType) },
		Keyword(Kw::Integer)  => { p.bump(); Ok(IntType) },
		Keyword(Kw::Time)     => { p.bump(); Ok(TimeType) },

		// Non-integer Types
		Keyword(Kw::Shortreal) => { p.bump(); Ok(ShortRealType) },
		Keyword(Kw::Real)      => { p.bump(); Ok(RealType) },
		Keyword(Kw::Realtime)  => { p.bump(); Ok(RealtimeType) },

		// Enumerations
		Keyword(Kw::Enum) => parse_enum_type(p),
		Keyword(Kw::Struct) | Keyword(Kw::Union) => parse_struct_type(p),

		// Named types
		Ident(n) | EscIdent(n) => { p.bump(); Ok(NamedType(n)) },

		// Virtual Interface Type
		Keyword(Kw::Virtual) => {
			p.bump();
			p.try_eat(Keyword(Kw::Interface));
			let (name, _) = p.eat_ident("virtual interface name")?;
			Ok(VirtIntfType(name))
		},

		_ => {
			let q = p.peek(0).1;
			p.add_diag(DiagBuilder2::error("Expected type").span(q));
			return Err(());
		}
	}
}


fn parse_enum_type(p: &mut AbstractParser) -> ReportedResult<TypeData> {
	// Consume the enum keyword.
	p.bump();

	// Parse the optional enum base type.
	let base = if p.peek(0).0 != OpenDelim(Brace) {
		Some(Box::new(parse_data_type(p)?))
	} else {
		None
	};

	// Parse the name declarations.
	let names = flanked(p, Brace, |p| comma_list(p, CloseDelim(Brace), "enum name", parse_enum_name))?;

	Ok(EnumType(base, names))
}


fn parse_enum_name(p: &mut AbstractParser) -> ReportedResult<EnumName> {
	// Eat the name.
	let (name, name_sp) = p.eat_ident("enum name")?;
	let mut span = name_sp;

	// Parse the optional range.
	let range = try_flanked(p, Brack, parse_expr)?;

	// Parse the optional value.
	let value = if p.try_eat(Operator(Op::Assign)) {
		Some(parse_expr(p)?)
	} else {
		None
	};
	span.expand(p.last_span());

	Ok(EnumName {
		span: span,
		name: name,
		name_span: name_sp,
		range: range,
		value: value,
	})
}


fn parse_struct_type(p: &mut AbstractParser) -> ReportedResult<TypeData> {
	let q = p.peek(0).1;

	// Consume the "struct", "union", or "union tagged" keywords.
	let kind = match (p.peek(0).0, p.peek(1).0) {
		(Keyword(Kw::Struct), _) => { p.bump(); StructKind::Struct },
		(Keyword(Kw::Union), Keyword(Kw::Tagged)) => { p.bump(); p.bump(); StructKind::TaggedUnion },
		(Keyword(Kw::Union), _) => { p.bump(); StructKind::Union },
		_ => {
			p.add_diag(DiagBuilder2::error("Expected `struct`, `union`, or `union tagged`").span(q));
			return Err(());
		}
	};

	// Consume the optional "packed" keyword, followed by an optional signing
	// indication.
	let (packed, signing) = if p.try_eat(Keyword(Kw::Packed)) {
		(true, parse_signing(p))
	} else {
		(false, TypeSign::None)
	};

	// Parse the struct members.
	let members = flanked(p, Brace, |p| repeat_until(p, CloseDelim(Brace), parse_struct_member))?;

	Ok(StructType {
		kind: kind,
		packed: packed,
		signing: signing,
		members: members,
	})
}


fn parse_struct_member(p: &mut AbstractParser) -> ReportedResult<StructMember> {
	let mut span = p.peek(0).1;

	// Parse the optional random qualifier.
	let rand_qualifier = match p.peek(0).0 {
		Keyword(Kw::Rand) => { p.bump(); Some(RandomQualifier::Rand) },
		Keyword(Kw::Randc) => { p.bump(); Some(RandomQualifier::Randc) },
		_ => None,
	};

	// Parse the data type of the member.
	let ty = parse_data_type(p)?;

	// Parse the list of names and assignments.
	let names = comma_list_nonempty(p, Semicolon, "member name", parse_variable_decl_assignment)?;

	p.require_reported(Semicolon)?;
	span.expand(p.last_span());

	Ok(StructMember {
		span: span,
		rand_qualifier: rand_qualifier,
		ty: Box::new(ty),
		names: names,
	})
}


fn parse_signing(p: &mut AbstractParser) -> TypeSign {
	match p.peek(0).0 {
		Keyword(Kw::Signed) => { p.bump(); TypeSign::Signed },
		Keyword(Kw::Unsigned) => { p.bump(); TypeSign::Unsigned },
		_ => TypeSign::None,
	}
}


fn parse_optional_dimensions(p: &mut AbstractParser) -> ReportedResult<(Vec<TypeDim>, Span)> {
	let mut v = Vec::new();
	let mut span;
	if let Some((d,sp)) = try_dimension(p)? {
		span = sp;
		v.push(d);
	} else {
		return Ok((v, INVALID_SPAN));
	}
	while let Some((d,sp)) = try_dimension(p)? {
		v.push(d);
		span.expand(sp);
	}
	Ok((v, span))
}


fn try_dimension(p: &mut AbstractParser) -> ReportedResult<Option<(TypeDim, Span)>> {
	// Eat the leading opening brackets.
	if !p.try_eat(OpenDelim(Brack)) {
		return Ok(None);
	}
	let mut span = p.last_span();

	let dim = match p.peek(0).0 {
		CloseDelim(Brack) => {
			p.bump();
			TypeDim::Unsized
		}
		Operator(Op::Mul) => {
			p.bump();
			TypeDim::Associative
		}
		Dollar => {
			p.bump();
			if p.try_eat(Colon) {
				Some(parse_expr(p)?)
			} else {
				None
			};
			TypeDim::Queue
		}
		_ => {
			// What's left must either be a single constant expression, or a range
			// consisting of two constant expressions.
			let expr = match parse_constant_expr(p) {
				Ok(x) => x,
				Err(_) => {
					p.recover_balanced(&[CloseDelim(Brack)], true);
					return Err(());
				}
			};

			// If the expression is followed by a colon `:`, this is a constant range
			// rather than a constant expression.
			if p.try_eat(Colon) {
				let other = match parse_constant_expr(p) {
					Ok(x) => x,
					Err(_) => {
						p.recover_balanced(&[CloseDelim(Brack)], true);
						return Err(());
					}
				};
				TypeDim::Range
			} else {
				TypeDim::Expr
			}
		}
	};

	// Eat the closing brackets.
	match p.peek(0) {
		(CloseDelim(Brack), sp) => {
			span.expand(sp);
			p.bump();
			return Ok(Some((dim, span)));
		},
		(tkn, sp) => {
			p.add_diag(DiagBuilder2::error(format!("Expected closing brackets `]` after dimension, got {}", tkn)).span(sp));
			p.recover_balanced(&[CloseDelim(Brack)], true);
			return Err(());
		}
	}
}


fn parse_list_of_port_connections(p: &mut AbstractParser) -> ReportedResult<Vec<()>> {
	let mut v = Vec::new();
	if p.peek(0).0 == CloseDelim(Paren) {
		return Ok(v);
	}
	loop {
		if p.try_eat(Period) {
			if p.try_eat(Operator(Op::Mul)) {
				// handle .* case
				let q = p.last_span();
				p.add_diag(DiagBuilder2::error("Don't know how to handle .* port connections").span(q));
			} else {
				let (name, name_sp) = p.eat_ident("port name")?;
				// handle .name, .name(), and .name(expr) cases
				if p.try_eat(OpenDelim(Paren)) {
					if !p.try_eat(CloseDelim(Paren)) {
						match parse_expr(p) {
							Ok(_) => (),
							Err(x) => {
								p.recover_balanced(&[CloseDelim(Paren)], false);
							},
						}
						p.require_reported(CloseDelim(Paren))?;
					}
				}
			}
		} else {
			// handle expr
			parse_expr(p)?;
		}

		// Depending on the next character, continue with the next port
		// connection or close the loop.
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if let (CloseDelim(Paren), _) = p.peek(0) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				} else {
					continue;
				}
			}
			(CloseDelim(Paren), _) => break,
			(x, sp) => {
				p.add_diag(DiagBuilder2::error(format!("Expected , or ) after list of port connections, got `{:?}`", x)).span(sp));
				return Err(());
			}
		}
	}

	Ok(v)
}


fn parse_expr(p: &mut AbstractParser) -> ReportedResult<Expr> {
	parse_expr_prec(p, Precedence::Min)
}

fn parse_expr_prec(p: &mut AbstractParser, precedence: Precedence) -> ReportedResult<Expr> {
	// Parse class-new and dynamic-array-new expressions, which are used on the
	// right hand side of assignments.
	if p.try_eat(Keyword(Kw::New)) {
		let mut span = p.last_span();
		if let Some(dim_expr) = try_flanked(p, Brack, parse_expr)? {
			let expr = try_flanked(p, Paren, parse_expr)?;
			span.expand(p.last_span());
			return Ok(Expr {
				span: span,
				data: ArrayNewExpr(Box::new(dim_expr), expr.map(|x| Box::new(x))),
			});
		} else {
			if let Some(args) = try_flanked(p, Paren, parse_call_args)? {
				span.expand(p.last_span());
				return Ok(Expr {
					span: span,
					data: ConstructorCallExpr(args),
				});
			} else {
				let expr = parse_expr(p)?;
				span.expand(p.last_span());
				return Ok(Expr {
					span: span,
					data: ClassNewExpr(Some(Box::new(expr))),
				});
			}
		}
	}

	// Otherwise treat this as a normal expression.
	let q = p.peek(0).1;
	// p.add_diag(DiagBuilder2::note(format!("expr_suffix with precedence {:?}", precedence)).span(q));
	let prefix = parse_expr_first(p, precedence)?;
	parse_expr_suffix(p, prefix, precedence)
}

fn parse_expr_suffix(p: &mut AbstractParser, prefix: Expr, precedence: Precedence) -> ReportedResult<Expr> {
	// p.add_diag(DiagBuilder2::note(format!("expr_suffix with precedence {:?}", precedence)).span(prefix.span));

	// Try to parse the index and call expressions.
	let (tkn, sp) = p.peek(0);
	match tkn {
		// Index: "[" range_expression "]"
		OpenDelim(Brack) if precedence <= Precedence::Scope => {
			p.bump();
			let expr = match parse_range_expr(p) {
				Ok(x) => x,
				Err(e) => {
					p.recover_balanced(&[CloseDelim(Brack)], true);
					return Err(e);
				}
			};
			p.require_reported(CloseDelim(Brack))?;
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}

		// Call: "(" [list_of_arguments] ")"
		OpenDelim(Paren) if precedence <= Precedence::Scope => {
			let args = flanked(p, Paren, parse_call_args);
			// p.add_diag(DiagBuilder2::warning("Don't know how to properly parse call expressions").span(sp));
			// p.recover_balanced(&[CloseDelim(Paren)], true);
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}

		// expr "." ident
		Period if precedence <= Precedence::Scope => {
			p.bump();
			p.eat_ident("member name")?;
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}

		// expr "::" ident
		Namespace if precedence <= Precedence::Scope => {
			p.bump();
			p.eat_ident("scope name")?;
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}

		// expr "++"
		Operator(Op::Inc) if precedence <= Precedence::Unary => {
			p.bump();
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}

		// expr "--"
		Operator(Op::Dec) if precedence <= Precedence::Unary => {
			p.bump();
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}

		// expr "?" expr ":" expr
		Ternary if precedence < Precedence::Ternary => {
			p.bump();
			let true_expr = parse_expr_prec(p, Precedence::Ternary)?;
			p.require_reported(Colon)?;
			let false_expr = parse_expr_prec(p, Precedence::Ternary)?;
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}

		_ => ()
	}

	// Try assign operators.
	if let Some(op) = as_assign_operator(tkn) {
		if precedence <= Precedence::Assignment {
			p.bump();
			parse_expr_prec(p, Precedence::Assignment)?;
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}
	}

	// Try to parse binary operations.
	if let Some(op) = as_binary_operator(tkn) {
		let prec = op.get_precedence();
		if precedence <= prec {
			p.bump();
			parse_expr_prec(p, prec)?;
			let expr = Expr {
				span: Span::union(prefix.span, p.last_span()),
				data: DummyExpr,
			};
			return parse_expr_suffix(p, expr, precedence);
		}
	}

	Ok(prefix)
}

fn parse_expr_first(p: &mut AbstractParser, precedence: Precedence) -> ReportedResult<Expr> {
	let first = p.peek(0).1;

	// Certain expressions are introduced by an operator or keyword. Handle
	// these cases first, since they are the quickest to decide.
	match p.peek(0) {
		(Operator(Op::Inc), _) if precedence <= Precedence::Unary => {
			p.bump();
			parse_expr_prec(p, Precedence::Unary)?;
			return Ok(Expr {
				span: Span::union(first, p.last_span()),
				data: DummyExpr,
			});
		}

		(Operator(Op::Dec), _) if precedence <= Precedence::Unary => {
			p.bump();
			parse_expr_prec(p, Precedence::Unary)?;
			return Ok(Expr {
				span: Span::union(first, p.last_span()),
				data: DummyExpr,
			});
		}

		(Keyword(Kw::Tagged), sp) => {
			p.add_diag(DiagBuilder2::error("Tagged union expressions not implemented").span(sp));
			return Err(());
		}

		_ => ()
	}

	// Try the unary operators next.
	if let Some(op) = as_unary_operator(p.peek(0).0) {
		p.bump();
		parse_expr_prec(p, Precedence::Unary)?;
		return Ok(Expr {
			span: Span::union(first, p.last_span()),
			data: DummyExpr,
		});
	}

	// Since none of the above matched, this must be a primary expression.
	parse_primary_expr(p)
}


fn parse_primary_expr(p: &mut AbstractParser) -> ReportedResult<Expr> {
	let (tkn, sp) = p.peek(0);
	match tkn {
		// Primary Literals
		UnsignedNumber(_) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}
		Literal(Lit::Str(..)) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}
		Literal(Lit::Decimal(..)) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}
		Literal(Lit::BasedInteger(..)) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}
		Literal(Lit::UnbasedUnsized(..)) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}

		// Identifiers
		Ident(_) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}
		EscIdent(_) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}
		SysIdent(_) => {
			p.bump();
			return Ok(Expr {
				span: sp,
				data: DummyExpr,
			});
		}

		// Concatenation and empty queue
		OpenDelim(Brace) => {
			p.bump();
			if p.try_eat(CloseDelim(Brace)) {
				// TODO: Handle empty queue.
				return Ok(Expr {
					span: Span::union(sp, p.last_span()),
					data: DummyExpr,
				});
			}
			match parse_concat_expr(p) {
				Ok(x) => x,
				Err(e) => {
					p.recover_balanced(&[CloseDelim(Brace)], true);
					return Err(e);
				}
			};
			p.require_reported(CloseDelim(Brace))?;
			return Ok(Expr {
				span: Span::union(sp, p.last_span()),
				data: DummyExpr,
			});
		}

		// Parenthesis
		OpenDelim(Paren) => {
			p.bump();
			let e = match parse_primary_parenthesis(p) {
				Ok(x) => x,
				Err(e) => {
					p.recover_balanced(&[CloseDelim(Paren)], true);
					return Err(e);
				}
			};
			p.require_reported(CloseDelim(Paren))?;
			return Ok(Expr {
				span: Span::union(sp, p.last_span()),
				data: DummyExpr,
			});
		}

		// Patterns
		Apostrophe => {
			p.bump();
			flanked(p, Brace, |p| comma_list_nonempty(p, CloseDelim(Brace), "pattern field", parse_pattern_field))?;
			return Ok(Expr {
				span: Span::union(sp, p.last_span()),
				data: DummyExpr,
			});
		}

		tkn => {
			p.add_diag(DiagBuilder2::error(format!("Expected expression, found {} instead", tkn)).span(sp));
			return Err(());
		}
	}
}


fn parse_pattern_field(p: &mut AbstractParser) -> ReportedResult<PatternField> {
	let mut span = p.peek(0).1;

	// Handle the trivial case of the "default" pattern.
	if p.try_eat(Keyword(Kw::Default)) {
		p.require_reported(Colon)?;
		let value = Box::new(parse_expr(p)?);
		span.expand(p.last_span());
		return Ok(PatternField {
			span: span,
			data: PatternFieldData::Default(value),
		});
	}

	// Otherwise handle the non-trivial cases.
	let mut pp = ParallelParser::new();

	// Try to parse expression patterns, which are of the form `expr ":" ...`.
	pp.add_greedy("expression pattern", |p|{
		let expr = Box::new(parse_expr(p)?);
		p.require_reported(Colon)?;
		let value = Box::new(parse_expr(p)?);
		Ok(PatternFieldData::Member(expr, value))
	});

	// Try to parse type patterns, which are of the form `type ":" ...`.
	pp.add_greedy("type pattern", |p|{
		let ty = parse_explicit_type(p)?;
		p.require_reported(Colon)?;
		let value = Box::new(parse_expr(p)?);
		Ok(PatternFieldData::Type(ty, value))
	});

	// ident ":"
	// expression ":"
	// type ":"
	// "default" ":"

	// expr
	// expr "{" expr {"," expr} "}"

	// Try to parse pattern fields that start with an expression, which may
	// either be a simple expression pattern or a repeat pattern.
	pp.add("expression or repeat pattern", |p|{
		let expr = Box::new(parse_expr(p)?);

		// If the expression is followed by an opening brace this is a repeat
		// pattern.
		let data = if let Some(inner_exprs) = try_flanked(p, Brace, |p| comma_list(p, CloseDelim(Brace), "expression", parse_expr))? {
			PatternFieldData::Repeat(expr, inner_exprs)
		} else {
			PatternFieldData::Expr(expr)
		};

		// Make sure this covers the whole pattern field.
		p.anticipate(&[Comma, CloseDelim(Brace)])?;
		Ok(data)
	});

	let data = pp.finish(p, "expression pattern")?;
	span.expand(p.last_span());
	Ok(PatternField {
		span: span,
		data: data,
	})
}


pub enum StreamDir {
	In,
	Out,
}

fn parse_concat_expr(p: &mut AbstractParser) -> ReportedResult<()> {
	/// Streaming concatenations have a "<<" or ">>" following the opening "{".
	let stream = match p.peek(0).0 {
		Operator(Op::LogicShL) => Some(StreamDir::Out),
		Operator(Op::LogicShR) => Some(StreamDir::In),
		_ => None
	};

	if let Some(dir) = stream {
		p.bump();

		// Parse the optional slice size. This can either be an expression or a
		// type. We prefer to parse things as expressions, and only if that does
		// not succeed do we switch to a type.
		let slice_size = if p.peek(0).0 != OpenDelim(Brace) {
			let mut pp = ParallelParser::new();
			pp.add_greedy("slice size expression", |p|{
				let s = parse_expr(p).map(|e| StreamConcatSlice::Expr(Box::new(e)))?;
				p.anticipate(&[OpenDelim(Brace)])?;
				Ok(s)
			});
			pp.add_greedy("slice size type", |p|{
				let s = parse_explicit_type(p).map(|t| StreamConcatSlice::Type(t))?;
				p.anticipate(&[OpenDelim(Brace)])?;
				Ok(s)
			});
			Some(pp.finish(p, "slice size expression or type")?)
		} else {
			None
		};

		// Parse the stream expressions.
		let exprs = flanked(p, Brace, |p| comma_list_nonempty(p, CloseDelim(Brace), "stream expression", |p|{
			// Consume the expression.
			let expr = Box::new(parse_expr(p)?);

			// Consume the optional range.
			let range = if p.try_eat(Keyword(Kw::With)) {
				Some(Box::new(flanked(p, Brack, parse_range_expr)?))
			} else {
				None
			};

			Ok(StreamExpr {
				expr: expr,
				range: range,
			})
		}))?;

		return Ok(());
		// let q = p.peek(0).1;
		// p.add_diag(DiagBuilder2::error("Don't know how to handle streaming concatenation").span(q));
		// return Err(());
	}

	// Parse the expression that follows the opening "{". Depending on whether
	// this is a regular concatenation or a multiple concatenation, the meaning
	// of the expression changes.
	let first_expr = parse_expr_prec(p, Precedence::Concatenation)?;

	// If the expression is followed by a "{", this is a multiple concatenation.
	if p.try_eat(OpenDelim(Brace)) {
		match parse_expr_list(p) {
			Ok(x) => x,
			Err(e) => {
				p.recover_balanced(&[CloseDelim(Brace)], true);
				return Err(e);
			}
		};
		p.require_reported(CloseDelim(Brace))?;
		return Ok(());
	}

	// Otherwise this is just a regular concatenation, so the first expression
	// may be followed by "," and another expression multiple times.
	while p.try_eat(Comma) {
		if p.peek(0).0 == CloseDelim(Brace) {
			let q = p.peek(0).1;
			p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(q));
			break;
		}
		parse_expr_prec(p, Precedence::Min)?;
	}

	Ok(())
}


fn parse_expr_list(p: &mut AbstractParser) -> ReportedResult<Vec<Expr>> {
	let mut v = Vec::new();
	loop {
		v.push(parse_expr_prec(p, Precedence::Min)?);

		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == CloseDelim(Brace) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(CloseDelim(Brace), _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or } after expression").span(sp));
				return Err(());
			}
		}
	}
	Ok(v)
}


/// Parse the tail of a primary expression that started with a parenthesis.
///
/// ## Syntax
/// ```
/// "(" expression ")"
/// "(" expression ":" expression ":" expression ")"
/// ```
fn parse_primary_parenthesis(p: &mut AbstractParser) -> ReportedResult<()> {
	parse_expr_prec(p, Precedence::Min)?;
	if p.try_eat(Colon) {
		parse_expr_prec(p, Precedence::Min)?;
		p.require_reported(Colon)?;
		parse_expr_prec(p, Precedence::Min)?;
	}
	return Ok(());
}


/// Parse a range expression.
///
/// ## Syntax
/// ```
/// expression
/// expression ":" expression
/// expression "+:" expression
/// expression "-:" expression
/// ```
fn parse_range_expr(p: &mut AbstractParser) -> ReportedResult<Expr> {
	let mut span = p.peek(0).1;
	let first_expr = parse_expr(p)?;
	let data = match p.peek(0).0 {
		Colon => {
			p.bump();
			parse_expr(p)?;
			DummyExpr
		}

		AddColon => {
			p.bump();
			parse_expr(p)?;
			DummyExpr
		}

		SubColon => {
			p.bump();
			parse_expr(p)?;
			DummyExpr
		}

		// Otherwise the index expression consists only of one expression.
		_ => {
			DummyExpr
		}
	};
	span.expand(p.last_span());
	Ok(Expr {
		span: span,
		data: data,
	})
}



/// Convert a token to the corresponding unary operator. Return `None` if the
/// token does not map to a unary operator.
fn as_unary_operator(tkn: Token) -> Option<Op> {
	if let Operator(op) = tkn {
		match op {
			Op::Add      |
			Op::Sub      |
			Op::LogicNot |
			Op::BitNot   |
			Op::BitAnd   |
			Op::BitNand  |
			Op::BitOr    |
			Op::BitNor   |
			Op::BitXor   |
			Op::BitNxor  |
			Op::BitXnor  => Some(op),
			_ => None,
		}
	} else {
		None
	}
}

/// Convert a token to the corresponding binary operator. Return `None` if the
/// token does not map to a binary operator.
fn as_binary_operator(tkn: Token) -> Option<Op> {
	if let Operator(op) = tkn {
		match op {
			Op::Add         |
			Op::Sub         |
			Op::Mul         |
			Op::Div         |
			Op::Mod         |
			Op::LogicEq     |
			Op::LogicNeq    |
			Op::CaseEq      |
			Op::CaseNeq     |
			Op::WildcardEq  |
			Op::WildcardNeq |
			Op::LogicAnd    |
			Op::LogicOr     |
			Op::Pow         |
			Op::Lt          |
			Op::Leq         |
			Op::Gt          |
			Op::Geq         |
			Op::BitAnd      |
			Op::BitOr       |
			Op::BitXor      |
			Op::BitXnor     |
			Op::BitNxor     |
			Op::LogicShL    |
			Op::LogicShR    |
			Op::ArithShL    |
			Op::ArithShR    |
			Op::LogicImpl   |
			Op::LogicEquiv  => Some(op),
			_ => None,
		}
	} else {
		None
	}
}

/// Convert a token to the corresponding AssignOp. Return `None` if the token
/// does not map to an assignment operator.
fn as_assign_operator(tkn: Token) -> Option<AssignOp> {
	match tkn {
		Operator(Op::Assign)         => Some(AssignOp::Identity),
		Operator(Op::AssignAdd)      => Some(AssignOp::Add),
		Operator(Op::AssignSub)      => Some(AssignOp::Sub),
		Operator(Op::AssignMul)      => Some(AssignOp::Mul),
		Operator(Op::AssignDiv)      => Some(AssignOp::Div),
		Operator(Op::AssignMod)      => Some(AssignOp::Mod),
		Operator(Op::AssignBitAnd)   => Some(AssignOp::BitAnd),
		Operator(Op::AssignBitOr)    => Some(AssignOp::BitOr),
		Operator(Op::AssignBitXor)   => Some(AssignOp::BitXor),
		Operator(Op::AssignLogicShL) => Some(AssignOp::LogicShL),
		Operator(Op::AssignLogicShR) => Some(AssignOp::LogicShR),
		Operator(Op::AssignArithShL) => Some(AssignOp::ArithShL),
		Operator(Op::AssignArithShR) => Some(AssignOp::ArithShR),
		_ => None,
	}
}


/// Parse a comma-separated list of ports, up to a closing parenthesis. Assumes
/// that the opening parenthesis has already been consumed.
fn parse_port_list(p: &mut AbstractParser) -> ReportedResult<Vec<Port>> {
	let mut v = Vec::new();

	// In case the port list is empty.
	if p.try_eat(CloseDelim(Paren)) {
		return Ok(v);
	}

	loop {
		// Parse a port.
		match parse_port(p, v.last()) {
			Ok(x) => v.push(x),
			Err(()) => p.recover_balanced(&[Comma, CloseDelim(Paren)], false)
		}

		// Depending on what follows, continue or break out of the loop.
		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == CloseDelim(Paren) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(CloseDelim(Paren), _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ) after port").span(sp));
				p.recover_balanced(&[CloseDelim(Paren)], false);
				break;
			}
		}
	}

	p.require_reported(CloseDelim(Paren))?;
	Ok(v)
}


/// Parse one port in a module or interface port list. The `prev` argument shall
/// be a reference to the previously parsed port, or `None` if this is the first
/// port in the list. This is required since ports inherit certain information
/// from their predecessor if omitted.
fn parse_port(p: &mut AbstractParser, prev: Option<&Port>) -> ReportedResult<Port> {
	let mut span = p.peek(0).1;

	// Consume the optional port direction.
	let mut dir = as_port_direction(p.peek(0).0);
	if dir.is_some() {
		p.bump();
	}

	// Consume the optional net type or var keyword, which determines the port
	// kind.
	let mut kind = match p.peek(0).0 {
		// Net Types
		Keyword(Kw::Supply0) => Some(NetPort),
		Keyword(Kw::Supply1) => Some(NetPort),
		Keyword(Kw::Tri)     => Some(NetPort),
		Keyword(Kw::Triand)  => Some(NetPort),
		Keyword(Kw::Trior)   => Some(NetPort),
		Keyword(Kw::Trireg)  => Some(NetPort),
		Keyword(Kw::Tri0)    => Some(NetPort),
		Keyword(Kw::Tri1)    => Some(NetPort),
		Keyword(Kw::Uwire)   => Some(NetPort),
		Keyword(Kw::Wire)    => Some(NetPort),
		Keyword(Kw::Wand)    => Some(NetPort),
		Keyword(Kw::Wor)     => Some(NetPort),

		// Var Kind
		Keyword(Kw::Var)     => Some(VarPort),
		_ => None
	};
	if kind.is_some() {
		p.bump();
	}

	// Try to parse ports of the form:
	// "." port_identifier "(" [expression] ")"
	if p.try_eat(Period) {
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Ports starting with a . not yet supported").span(q));
		return Err(())
	}

	// Otherwise parse the port data type, which may be a whole host of
	// different things.
	let mut ty = Some(parse_data_type(p)?);

	// Here goes the tricky part: If the data type not followed by the name (and
	// optional dimensions) of the port, the data type actually was the port
	// name. These are indistinguishable.
	let (name, name_span, (dims, dims_span)) = if let Some((name, span)) = p.try_eat_ident() {
		(name, span, parse_optional_dimensions(p)?)
	} else {
		if let Some(Type { span: span, data: NamedType(name), dims: dims, .. }) = ty {
			let r = (name, span, (dims, span));
			ty = None;
			r
		} else {
			p.add_diag(DiagBuilder2::error("Expected port type or name").span(ty.unwrap().span));
			return Err(());
		}
	};

	// Determine the kind of the port based on the optional kind keywords, the
	// direction, and the type.
	if dir.is_none() && kind.is_none() && ty.is_none() && prev.is_some() {
		dir = Some(prev.unwrap().dir.clone());
		kind = Some(prev.unwrap().kind.clone());
		ty = Some(prev.unwrap().ty.clone());
	} else {
		// The direction defaults to inout.
		if dir.is_none() {
			dir = Some(PortDir::Inout);
		}

		// The type defaults to logic.
		if ty.is_none() {
			ty = Some(Type {
				span: INVALID_SPAN,
				data: LogicType,
				sign: TypeSign::None,
				dims: Vec::new(),
			});
		}

		// The kind defaults to different things based on the direction and
		// type:
		// - input,inout: default net
		// - ref: var
		// - output (implicit type): net
		// - output (explicit type): var
		if kind.is_none() {
			kind = Some(match dir.unwrap() {
				PortDir::Input | PortDir::Inout => NetPort,
				PortDir::Ref => VarPort,
				PortDir::Output if ty.clone().unwrap().data == ImplicitType => NetPort,
				PortDir::Output => VarPort,
			});
		}
	}

	// Parse the optional initial assignment for this port.
	if p.try_eat(Operator(Op::Assign)) {
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Ports with initial assignment not yet supported").span(q));
	}

	// Update the port's span to cover all of the tokens consumed.
	span.expand(p.last_span());

	Ok(Port {
		span: span,
		name: name,
		name_span: name_span,
		kind: kind.unwrap(),
		ty: ty.unwrap(),
		dir: dir.unwrap(),
		dims: dims,
	})
}


fn parse_parameter_assignments(p: &mut AbstractParser) -> ReportedResult<Vec<()>> {
	let mut v = Vec::new();
	p.require_reported(OpenDelim(Paren))?;

	// In case there are no parameter assignments, the opening parenthesis is
	// directly followed by a closing one.
	if p.try_eat(CloseDelim(Paren)) {
		return Ok(v);
	}

	loop {
		match parse_parameter_assignment(p) {
			Ok(x) => v.push(x),
			Err(()) => p.recover_balanced(&[Comma, CloseDelim(Paren)], false)
		}

		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == CloseDelim(Paren) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(CloseDelim(Paren), _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ) after parameter assignment, found").span(sp));
				p.recover_balanced(&[CloseDelim(Paren)], false);
				break;
			}
		}
	}

	p.require_reported(CloseDelim(Paren))?;
	Ok(v)
}


fn parse_parameter_assignment(p: &mut AbstractParser) -> ReportedResult<()> {
	// If the parameter assignment starts with a ".", this is a named
	// assignment. Otherwise it's an ordered assignment.
	if p.try_eat(Period) {
		let (name, name_span) = p.eat_ident("parameter name")?;
		p.require_reported(OpenDelim(Paren))?;
		let expr = match parse_expr(p) {
			Ok(x) => x,
			Err(()) => {
				p.recover_balanced(&[CloseDelim(Paren)], true);
				return Err(());
			}
		};
		p.require_reported(CloseDelim(Paren))?;
		// println!("named param assignment: {} = {:?}", name, expr);
		Ok(())
	} else {
		let expr = parse_expr(p)?;
		// println!("ordered param assignment: {:?}", expr);
		Ok(())
	}
}


fn parse_procedure(p: &mut AbstractParser, kind: ProcedureKind) -> ReportedResult<Procedure> {
	p.bump();
	let mut span = p.last_span();
	let stmt = parse_stmt(p)?;
	span.expand(p.last_span());
	Ok(Procedure {
		span: span,
		kind: kind,
		stmt: stmt,
	})
}


fn parse_subroutine_decl(p: &mut AbstractParser) -> ReportedResult<SubroutineDecl> {
	let mut span = p.peek(0).1;

	// Consume the subroutine prototype, which covers everything up to the ";"
	// after the argument list.
	let prototype = parse_subroutine_prototype(p)?;

	// Consume the subroutine body, which basically is a list of statements.
	let term = match prototype.kind {
		SubroutineKind::Func => Keyword(Kw::Endfunction),
		SubroutineKind::Task => Keyword(Kw::Endtask),
	};
	let items = repeat_until(p, term, parse_subroutine_item)?;

	// Consume the "endfunction" or "endtask" keywords.
	p.require_reported(term)?;

	span.expand(p.last_span());
	Ok(SubroutineDecl {
		span: span,
		prototype: prototype,
		items: items,
	})
}


fn parse_subroutine_prototype(p: &mut AbstractParser) -> ReportedResult<SubroutinePrototype> {
	let mut span = p.peek(0).1;

	// Consume the "function" or "task" keyword, which then also decides what
	// kind of subroutine we're parsing.
	let kind = match p.peek(0).0 {
		Keyword(Kw::Function) => { p.bump(); SubroutineKind::Func },
		Keyword(Kw::Task)     => { p.bump(); SubroutineKind::Task },
		_ => {
			p.add_diag(DiagBuilder2::error("Expected function or task prototype").span(span));
			return Err(());
		}
	};

	// Parse the return type (if this is a function), the subroutine name, and
	// the optional argument list.
	let (retty, (name, name_span, args)) = if kind == SubroutineKind::Func {
		if p.peek(0).0 == Keyword(Kw::New) {
			(None, parse_subroutine_prototype_tail(p)?)
		} else {
			let mut pp = ParallelParser::new();
			pp.add("implicit function return type", |p|{
				let ty = parse_implicit_type(p)?;
				Ok((Some(ty), parse_subroutine_prototype_tail(p)?))
			});
			pp.add("explicit function return type", |p|{
				let ty = parse_explicit_type(p)?;
				Ok((Some(ty), parse_subroutine_prototype_tail(p)?))
			});
			pp.finish(p, "implicit or explicit function return type")?
		}
	} else {
		(None, parse_subroutine_prototype_tail(p)?)
	};

	span.expand(p.last_span());
	Ok(SubroutinePrototype {
		span: span,
		kind: kind,
		name: name,
		name_span: name_span,
		args: args,
	})
}


fn parse_subroutine_prototype_tail(p: &mut AbstractParser) -> ReportedResult<(Name, Span, Vec<SubroutinePort>)> {
	// Consume the subroutine name, or "new".
	// TODO: Make this accept the full `[interface_identifier "." | class_scope] tf_identifier`.
	let (name, name_span) = if p.try_eat(Keyword(Kw::New)) {
		(get_name_table().intern("new", true), p.last_span())
	} else {
		p.eat_ident("function or task name")?
	};

	// Consume the port list.
	let args = try_flanked(p, Paren, |p| comma_list(p, CloseDelim(Paren), "subroutine port", |p|{
		let mut span = p.peek(0).1;

		// Consume the optional port direction.
		let dir = try_subroutine_port_dir(p);

		// Consume the optional "var" keyword.
		let var = p.try_eat(Keyword(Kw::Var));

		// Branch to parse ports with explicit and implicit type.
		let mut pp = ParallelParser::new();
		pp.add("explicit type", |p|{
			let ty = parse_explicit_type(p)?;
			Ok((ty, tail(p)?))
		});
		pp.add("implicit type", |p|{
			let ty = parse_implicit_type(p)?;
			Ok((ty, tail(p)?))
		});
		let (ty, name) = pp.finish(p, "explicit or implicit type")?;

		// The `tail` function handles everything that follows the data type. To
		// ensure that the ports are parsed correctly, the function must fail if
		// the port is not immediately followed by a "," or ")". Otherwise
		// implicit and explicit types cannot be distinguished.
		fn tail(p: &mut AbstractParser) -> ReportedResult<Option<SubroutinePortName>> {
			// Parse the optional port identifier.
			let data = if let Some((name, name_span)) = p.try_eat_ident() {
				// Parse the optional dimensions.
				let (dims, _) = parse_optional_dimensions(p)?;

				// Parse the optional initial assignment.
				let expr = if p.try_eat(Operator(Op::Assign)) {
					Some(parse_expr(p)?)
				} else {
					None
				};

				Some(SubroutinePortName {
					name: name,
					name_span: name_span,
					dims: dims,
					expr: expr,
				})
			} else {
				None
			};

			// Ensure that we have consumed all tokens for this port.
			match p.peek(0) {
				(Comma,_) | (CloseDelim(Paren),_) => Ok(data),
				(_, sp) => {
					p.add_diag(DiagBuilder2::error("Expected , or ) after subroutine port").span(sp));
					Err(())
				}
			}
		}

		span.expand(p.last_span());
		Ok(SubroutinePort {
			span: span,
			dir: dir,
			var: var,
			ty: ty,
			name: name,
		})
	}))?.unwrap_or(Vec::new());

	// Wrap things up.
	p.require_reported(Semicolon)?;
	Ok((name, name_span, args))
}


fn try_subroutine_port_dir(p: &mut AbstractParser) -> Option<SubroutinePortDir> {
	match (p.peek(0).0, p.peek(1).0) {
		(Keyword(Kw::Input),  _) => { p.bump(); Some(SubroutinePortDir::Input) },
		(Keyword(Kw::Output), _) => { p.bump(); Some(SubroutinePortDir::Output) },
		(Keyword(Kw::Inout),  _) => { p.bump(); Some(SubroutinePortDir::Inout) },
		(Keyword(Kw::Ref),    _) => { p.bump(); Some(SubroutinePortDir::Ref) },
		(Keyword(Kw::Const), Keyword(Kw::Ref)) => { p.bump(); p.bump(); Some(SubroutinePortDir::ConstRef) },
		_ => None,
	}
}


fn parse_subroutine_item(p: &mut AbstractParser) -> ReportedResult<SubroutineItem> {
	let mut span = p.peek(0).1;

	// Try to parse a port declaration of the form:
	// direction ["var"] type_or_implicit [name_assignment {"," name_assignment}]
	if let Some(dir) = try_subroutine_port_dir(p) {

		// Consume the optional "var" keyword.
		let var = p.try_eat(Keyword(Kw::Var));

		// Branch to handle the cases of implicit and explicit data type.
		let mut pp = ParallelParser::new();
		pp.add("explicit type", |p|{
			let ty = parse_explicit_type(p)?;
			let names = comma_list_nonempty(p, Semicolon, "port declaration", parse_variable_decl_assignment)?;
			p.require_reported(Semicolon)?;
			Ok((ty, names))
		});
		pp.add("implicit type", |p|{
			let ty = parse_implicit_type(p)?;
			let names = comma_list_nonempty(p, Semicolon, "port declaration", parse_variable_decl_assignment)?;
			p.require_reported(Semicolon)?;
			Ok((ty, names))
		});
		let (ty, names) = pp.finish(p, "explicit or implicit type")?;

		// Wrap things up.
		span.expand(p.last_span());
		return Ok(SubroutineItem::PortDecl(SubroutinePortDecl {
			span: span,
			dir: dir,
			var: var,
			ty: ty,
			names: names,
		}));
	}

	// Otherwise simply treat this as a statement.
	Ok(SubroutineItem::Stmt(parse_stmt(p)?))
}


fn parse_stmt(p: &mut AbstractParser) -> ReportedResult<Stmt> {
	let mut span = p.peek(0).1;

	// Null statements simply consist of a semicolon.
	if p.try_eat(Semicolon) {
		return Ok(Stmt::new_null(span));
	}

	// Consume the optional statement label.
	let mut label = if p.is_ident() && p.peek(1).0 == Colon {
		let (n,_) = p.eat_ident("statement label")?;
		p.bump(); // eat the colon
		Some(n)
	} else {
		None
	};

	// Parse the actual statement item.
	let data = parse_stmt_data(p, &mut label)?;
	span.expand(p.last_span());

	Ok(Stmt {
		span: span,
		label: label,
		data: data,
	})
}

fn parse_stmt_data(p: &mut AbstractParser, label: &mut Option<Name>) -> ReportedResult<StmtData> {
	let (tkn, sp) = p.peek(0);

	// See if this is a timing-controlled statement as per IEEE 1800-2009
	// section 9.4.
	if let Some(dc) = try_delay_control(p)? {
		let stmt = Box::new(parse_stmt(p)?);
		return Ok(TimedStmt(TimingControl::Delay(dc), stmt));
	}
	if let Some(ec) = try_event_control(p)? {
		let stmt = Box::new(parse_stmt(p)?);
		return Ok(TimedStmt(TimingControl::Event(ec), stmt));
	}
	if let Some(cd) = try_cycle_delay(p)? {
		let stmt = Box::new(parse_stmt(p)?);
		return Ok(TimedStmt(TimingControl::Cycle(cd), stmt));
	}

	Ok(match tkn {
		// Sequential blocks
		OpenDelim(Bgend) => {
			p.bump();
			let (stmts, _) = parse_block(p, label, &[CloseDelim(Bgend)])?;
			SequentialBlock(stmts)
		}

		// Parallel blocks
		Keyword(Kw::Fork) => {
			p.bump();
			let (stmts, terminator) = parse_block(p, label, &[Keyword(Kw::Join), Keyword(Kw::JoinAny), Keyword(Kw::JoinNone)])?;
			let join = match terminator {
				Keyword(Kw::Join) => JoinKind::All,
				Keyword(Kw::JoinAny) => JoinKind::Any,
				Keyword(Kw::JoinNone) => JoinKind::None,
				x => panic!("Invalid parallel block terminator {:?}", x),
			};
			ParallelBlock(stmts, join)
		}

		// If and case statements
		Keyword(Kw::Unique)   => { p.bump(); parse_if_or_case(p, Some(UniquePriority::Unique))? }
		Keyword(Kw::Unique0)  => { p.bump(); parse_if_or_case(p, Some(UniquePriority::Unique0))? }
		Keyword(Kw::Priority) => { p.bump(); parse_if_or_case(p, Some(UniquePriority::Priority))? }
		Keyword(Kw::If) | Keyword(Kw::Case) | Keyword(Kw::Casex) | Keyword(Kw::Casez) => parse_if_or_case(p, None)?,

		// Loops, as per IEEE 1800-2009 section 12.7.
		Keyword(Kw::Forever) => {
			p.bump();
			let stmt = Box::new(parse_stmt(p)?);
			ForeverStmt(stmt)
		}
		Keyword(Kw::Repeat) => {
			p.bump();
			let expr = flanked(p, Paren, parse_expr)?;
			let stmt = Box::new(parse_stmt(p)?);
			RepeatStmt(expr, stmt)
		}
		Keyword(Kw::While) => {
			p.bump();
			let expr = flanked(p, Paren, parse_expr)?;
			let stmt = Box::new(parse_stmt(p)?);
			WhileStmt(expr, stmt)
		}
		Keyword(Kw::Do) => {
			p.bump();
			let stmt = Box::new(parse_stmt(p)?);
			let q = p.last_span();
			if !p.try_eat(Keyword(Kw::While)) {
				p.add_diag(DiagBuilder2::error("Do loop requires a while clause").span(q));
				return Err(());
			}
			let expr = flanked(p, Paren, parse_expr)?;
			DoStmt(stmt, expr)
		}
		Keyword(Kw::For) => {
			p.bump();
			let (init, cond, step) = flanked(p, Paren, |p| {
				let init = Box::new(parse_stmt(p)?);
				let cond = parse_expr(p)?;
				p.require_reported(Semicolon)?;
				let step = parse_expr(p)?;
				Ok((init, cond, step))
			})?;
			let stmt = Box::new(parse_stmt(p)?);
			ForStmt(init, cond, step, stmt)
		}
		Keyword(Kw::Foreach) => {
			p.bump();
			let expr = flanked(p, Paren, parse_expr)?;
			let stmt = Box::new(parse_stmt(p)?);
			ForeachStmt(expr, stmt)
		}

		// Generate variables
		Keyword(Kw::Genvar) => {
			p.bump();
			let names = comma_list_nonempty(p, Semicolon, "genvar declaration", parse_genvar_decl)?;
			p.require_reported(Semicolon)?;
			GenvarDeclStmt(names)
		}

		// Flow control
		Keyword(Kw::Return) => {
			p.bump();
			ReturnStmt(
				if p.try_eat(Semicolon) {
					None
				} else {
					let expr = parse_expr(p)?;
					p.require_reported(Semicolon)?;
					Some(expr)
				}
			)
		}
		Keyword(Kw::Break) => { p.bump(); p.require_reported(Semicolon); BreakStmt }
		Keyword(Kw::Continue) => { p.bump(); p.require_reported(Semicolon); ContinueStmt }

		// Import statements
		Keyword(Kw::Import) => ImportStmt(parse_import_decl(p)?),

		// Assertion statements
		Keyword(Kw::Assert) |
		Keyword(Kw::Assume) |
		Keyword(Kw::Cover) |
		Keyword(Kw::Expect) |
		Keyword(Kw::Restrict) => AssertionStmt(Box::new(parse_assertion(p)?)),

		// Wait statements
		Keyword(Kw::Wait) => {
			p.bump();
			match p.peek(0) {
				(OpenDelim(Paren), _) => {
					let expr = flanked(p, Paren, parse_expr)?;
					let stmt = Box::new(parse_stmt(p)?);
					WaitExprStmt(expr, stmt)
				}
				(Keyword(Kw::Fork), _) => {
					p.bump();
					p.require_reported(Semicolon)?;
					WaitForkStmt
				}
				(tkn, sp) => {
					p.add_diag(DiagBuilder2::error(format!("Expected (<expr>) or fork after wait, found {} instead", tkn)).span(sp));
					return Err(());
				}
			}
		}
		Keyword(Kw::WaitOrder) => {
			p.add_diag(DiagBuilder2::error("Don't know how to parse wait_order statements").span(sp));
			return Err(());
		}

		// Disable statements
		Keyword(Kw::Disable) => {
			p.bump();
			if p.try_eat(Keyword(Kw::Fork)) {
				p.require_reported(Semicolon)?;
				DisableForkStmt
			} else {
				let (name, _) = p.eat_ident("task or block name")?;
				p.require_reported(Semicolon)?;
				DisableStmt(name)
			}
		}

		// Everything else needs special treatment as things such as variable
		// declarations look very similar to other expressions.
		_ => {
			let result = {
				let mut pp = ParallelParser::new();
				pp.add("variable declaration", |p|{
					let konst = p.try_eat(Keyword(Kw::Const));
					let var = p.try_eat(Keyword(Kw::Var));
					let lifetime = match as_lifetime(p.peek(0).0) {
						Some(x) => { p.bump(); x },
						None => Lifetime::Static,
					};
					let ty = parse_data_type(p)?;
					let decls = comma_list_nonempty(p, Semicolon, "variable declaration", parse_variable_decl_assignment)?;
					p.require_reported(Semicolon)?;
					Ok(NullStmt)
				});
				pp.add("assign statement", |p| parse_assign_stmt(p));
				pp.add("expression statement", |p| parse_expr_stmt(p));
				pp.finish(p, "statement")
			};
			match result {
				Ok(x) => x,
				Err(_) => {
					p.recover_balanced(&[Semicolon], true);
					return Err(())
				}
			}
		}
	})
}


fn parse_block(p: &mut AbstractParser, label: &mut Option<Name>, terminators: &[Token]) -> ReportedResult<(Vec<Stmt>, Token)> {
	let span = p.last_span();

	// Consume the optional block label. If the block has already been labelled
	// via a statement label, an additional block label is illegal.
	if p.try_eat(Colon) {
		let (name, name_span) = p.eat_ident("block label")?;
		if let Some(existing) = *label {
			if name == existing {
				p.add_diag(DiagBuilder2::warning(format!("Block {} labelled twice", name)).span(name_span));
			} else {
				p.add_diag(DiagBuilder2::error(format!("Block has been given two conflicting labels, {} and {}", existing, name)).span(name_span));
			}
		} else {
			*label = Some(name);
		}
	}

	// Parse the block statements.
	let mut v = Vec::new();
	let terminator;
	'outer: loop {
		// Check if we have reached one of the terminators.
		let tkn = p.peek(0).0;
		for term in terminators {
			if tkn == *term {
				terminator = *term;
				p.bump();
				break 'outer;
			}
		}

		// Otherwise parse the next statement.
		match parse_stmt(p) {
			Ok(x) => v.push(x),
			Err(()) => {
				p.recover_balanced(terminators, false);
				terminator = p.peek(0).0;
				p.bump();
				break;
			}
		}
	}

	// Consume the optional block label after the terminator and verify that it
	// matches the label provided at the beginning of the block.
	if p.try_eat(Colon) {
		let (name, name_span) = p.eat_ident("block label")?;
		if let Some(before) = *label {
			if before != name {
				p.add_diag(DiagBuilder2::error(format!("Block label {} at end of block does not match label {} at beginning of block", name, before)).span(name_span));
			}
		} else {
			p.add_diag(DiagBuilder2::error(format!("Block label {} provided at the end of the block, but not at the beginning", name)).span(name_span));
		}
	}

	Ok((v, terminator))
}


/// Parse a continuous assignment as per IEEE 1800-2009 section 10.3.
fn parse_continuous_assign(p: &mut AbstractParser) -> ReportedResult<()> {
	p.bump();
	let mut span = p.last_span();

	// Parse the optional delay control.
	try_delay_control(p)?;

	// Parse the optional drive strength.

	// Parse the optional delay triple.

	// Parse the list of assignments.
	loop {
		match parse_assignment(p) {
			Ok(x) => (),
			Err(()) => p.recover_balanced(&[Comma, Semicolon], false),
		}

		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.peek(0).0 == Semicolon {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			}
			(Semicolon, _) => break,
			(Eof, _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ; after assignment").span(sp));
				p.recover_balanced(&[Comma, Semicolon], false);
				break;
			}
		}
	}

	p.require_reported(Semicolon)?;
	span.expand(p.last_span());
	Ok(())
}


fn parse_if_or_case(p: &mut AbstractParser, up: Option<UniquePriority>) -> ReportedResult<StmtData> {
	let (tkn, span) = p.peek(0);
	match tkn {
		// Case statements
		Keyword(Kw::Case)  => { p.bump(); parse_case(p, up, CaseKind::Normal) },
		Keyword(Kw::Casez) => { p.bump(); parse_case(p, up, CaseKind::DontCareZ) },
		Keyword(Kw::Casex) => { p.bump(); parse_case(p, up, CaseKind::DontCareXZ) },

		// If statement
		Keyword(Kw::If) => { p.bump(); parse_if(p, up) },

		x => {
			p.add_diag(DiagBuilder2::error(format!("Expected case or if statement, got {:?}", x)).span(span));
			Err(())
		}
	}
}


/// Parse a case statement as per IEEE 1800-2009 section 12.5.
fn parse_case(p: &mut AbstractParser, up: Option<UniquePriority>, kind: CaseKind) -> ReportedResult<StmtData> {
	let q = p.last_span();

	// Parse the case expression.
	p.require_reported(OpenDelim(Paren))?;
	let expr = match parse_expr(p) {
		Ok(x) => x,
		Err(()) => {
			p.recover_balanced(&[CloseDelim(Paren)], true);
			return Err(());
		}
	};
	p.require_reported(CloseDelim(Paren))?;

	// The case expression may be followed by a "matches" or "inside" keyword
	// which changes the kind of operation the statement performs.
	let mode = match p.peek(0).0 {
		Keyword(Kw::Inside) => { p.bump(); CaseMode::Inside },
		Keyword(Kw::Matches) => { p.bump(); CaseMode::Pattern },
		_ => CaseMode::Normal,
	};

	// Parse the case items.
	let mut items = Vec::new();
	while p.peek(0).0 != Keyword(Kw::Endcase) && p.peek(0).0 != Eof {
		let mut span = p.peek(0).1;

		// Handle the default case items.
		if p.peek(0).0 == Keyword(Kw::Default) {
			p.bump();
			p.try_eat(Colon);
			let stmt = Box::new(parse_stmt(p)?);
			items.push(CaseItem::Default(stmt));
		}

		// Handle regular case items.
		else {
			let mut exprs = Vec::new();
			loop {
				match parse_expr(p) {
					Ok(x) => exprs.push(x),
					Err(()) => {
						p.recover_balanced(&[Colon], false);
						break;
					}
				}

				match p.peek(0) {
					(Comma, sp) => {
						p.bump();
						if p.try_eat(Colon) {
							p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
							break;
						}
					},
					(Colon, _) => break,
					(_, sp) => {
						p.add_diag(DiagBuilder2::error("Expected , or : after case expression").span(sp));
						break;
					}
				}
			}

			// Parse the statement.
			p.require_reported(Colon)?;
			let stmt = Box::new(parse_stmt(p)?);
			items.push(CaseItem::Expr(exprs, stmt));
		}
	}

	p.require_reported(Keyword(Kw::Endcase))?;

	Ok(CaseStmt {
		up: up,
		kind: kind,
		expr: expr,
		mode: mode,
		items: items,
	})
}


fn parse_if(p: &mut AbstractParser, up: Option<UniquePriority>) -> ReportedResult<StmtData> {
	// Parse the condition expression surrounded by parenthesis.
	p.require_reported(OpenDelim(Paren))?;
	let cond = match parse_expr(p) {
		Ok(x) => x,
		Err(()) => {
			p.recover_balanced(&[CloseDelim(Paren)], true);
			return Err(());
		}
	};
	p.require_reported(CloseDelim(Paren))?;

	// Parse the main statement.
	let main_stmt = Box::new(parse_stmt(p)?);

	// Parse the optional "else" branch.
	let else_stmt = if p.peek(0).0 == Keyword(Kw::Else) {
		p.bump();
		Some(Box::new(parse_stmt(p)?))
	} else {
		None
	};

	Ok(IfStmt {
		up: up,
		cond: cond,
		main_stmt: main_stmt,
		else_stmt: else_stmt,
	})
}


fn try_delay_control(p: &mut AbstractParser) -> ReportedResult<Option<DelayControl>> {
	// Try to consume the hashtag which introduces the delay control.
	if !p.try_eat(Hashtag) {
		return Ok(None);
	}
	let mut span = p.last_span();

	// Parse the delay value. This may either be a literal delay value,
	// or a min-typ-max expression in parenthesis.
	let (tkn, sp) = p.peek(0);
	let expr = match tkn {
		// Expression
		OpenDelim(Paren) => {
			p.bump();
			let e = parse_expr_prec(p, Precedence::MinTypMax)?;
			p.require_reported(CloseDelim(Paren))?;
			e
		}

		// Literals
		// TODO: Add real and time literals
		UnsignedNumber(..) |
		Literal(Real(..)) |
		Literal(Time(..)) => parse_expr_first(p, Precedence::Max)?,

		// TODO: Parse "1step" keyword
		_ => {
			p.add_diag(DiagBuilder2::error("Expected delay value or expression after #").span(sp));
			return Err(());
		}
	};
	span.expand(p.last_span());

	Ok(Some(DelayControl {
		span: span,
		expr: expr,
	}))
}

/// Try to parse an event control as described in IEEE 1800-2009 section 9.4.2.
fn try_event_control(p: &mut AbstractParser) -> ReportedResult<Option<EventControl>> {
	if !p.try_eat(At) {
		return Ok(None)
	}
	let mut span = p.last_span();

	// @* and @ (*)
	if p.peek(0).0 == Operator(Op::Mul) {
		p.bump();
		span.expand(p.last_span());
		return Ok(Some(EventControl {
			span: span,
			data: EventControlData::Implicit,
		}));
	}
	if p.peek(0).0 == OpenDelim(Paren) && p.peek(1).0 == Operator(Op::Mul) && p.peek(2).0 == CloseDelim(Paren) {
		p.bump();
		p.bump();
		p.bump();
		span.expand(p.last_span());
		return Ok(Some(EventControl {
			span: span,
			data: EventControlData::Implicit,
		}));
	}

	let expr = parse_event_expr(p, EventPrecedence::Max)?;
	span.expand(p.last_span());

	Ok(Some(EventControl {
		span: span,
		data: EventControlData::Expr(expr),
	}))
}

fn try_cycle_delay(p: &mut AbstractParser) -> ReportedResult<Option<CycleDelay>> {
	if !p.try_eat(DoubleHashtag) {
		return Ok(None)
	}

	let q = p.last_span();
	p.add_diag(DiagBuilder2::error("Don't know how to parse cycle delay").span(q));
	Err(())
}


fn parse_assignment(p: &mut AbstractParser) -> ReportedResult<(Expr, Expr)> {
	let lhs = parse_expr_prec(p, Precedence::Scope)?;
	p.require_reported(Operator(Op::Assign))?;
	let rhs = parse_expr_prec(p, Precedence::Assignment)?;
	Ok((lhs, rhs))
}


fn parse_assign_stmt(p: &mut AbstractParser) -> ReportedResult<StmtData> {
	// Parse the leading expression.
	let expr = parse_expr_prec(p, Precedence::Scope)?;
	let (tkn, sp) = p.peek(0);

	// Handle blocking assignments (IEEE 1800-2009 section 10.4.1), where the
	// expression is followed by an assignment operator.
	if let Some(op) = as_assign_operator(tkn) {
		p.bump();
		let rhs = parse_expr(p)?;
		p.require_reported(Semicolon)?;
		return Ok(BlockingAssignStmt {
			lhs: expr,
			rhs: rhs,
			op: op,
		});
	}

	// Handle non-blocking assignments (IEEE 1800-2009 section 10.4.2).
	if tkn == Operator(Op::Leq) {
		p.bump();

		// Parse the optional delay and event control.
		let delay_control = try_delay_control(p)?;
		let event_control = /*try_event_control(p)?*/ None;

		// Parse the right-hand side of the assignment.
		let rhs = parse_expr(p)?;
		p.require_reported(Semicolon)?;

		return Ok(NonblockingAssignStmt {
			lhs: expr,
			rhs: rhs,
			delay: delay_control,
			event: event_control,
		});
	}

	p.add_diag(DiagBuilder2::error("Expected blocking or non-blocking assign statement").span(sp));
	Err(())
}


fn parse_expr_stmt(p: &mut AbstractParser) -> ReportedResult<StmtData> {
	let expr = parse_expr_prec(p, Precedence::Unary)?;
	p.require_reported(Semicolon)?;
	Ok(ExprStmt(expr))
}


fn parse_event_expr(p: &mut AbstractParser, precedence: EventPrecedence) -> ReportedResult<EventExpr> {
	let mut span = p.peek(0).1;

	// Try parsing an event expression in parentheses.
	if p.try_eat(OpenDelim(Paren)) {
		return match parse_event_expr(p, EventPrecedence::Min) {
			Ok(x) => {
				p.require_reported(CloseDelim(Paren))?;
				parse_event_expr_suffix(p, x, precedence)
			}
			Err(()) => {
				p.recover_balanced(&[CloseDelim(Paren)], true);
				Err(())
			}
		};
	}

	// Consume the optional edge identifier.
	let edge = as_edge_ident(p.peek(0).0);
	if edge != EdgeIdent::Implicit {
		p.bump();
	}

	// Parse the value.
	let value = parse_expr(p)?;
	span.expand(p.last_span());

	let expr = EventExpr::Edge {
		span: span,
		edge: edge,
		value: value,
	};
	parse_event_expr_suffix(p, expr, precedence)

	// p.add_diag(DiagBuilder2::error("Expected event expression").span(span));
	// Err(())
}


fn parse_event_expr_suffix(p: &mut AbstractParser, expr: EventExpr, precedence: EventPrecedence) -> ReportedResult<EventExpr> {
	match p.peek(0).0 {
		// event_expr "iff" expr
		Keyword(Kw::Iff) if precedence < EventPrecedence::Iff => {
			p.bump();
			let cond = parse_expr(p)?;
			Ok(EventExpr::Iff {
				span: Span::union(expr.span(), cond.span),
				expr: Box::new(expr),
				cond: cond,
			})
		}
		// event_expr "or" event_expr
		// event_expr "," event_expr
		Keyword(Kw::Or) | Comma if precedence <= EventPrecedence::Or => {
			p.bump();
			let rhs = parse_event_expr(p, EventPrecedence::Or)?;
			Ok(EventExpr::Or {
				span: Span::union(expr.span(), rhs.span()),
				lhs: Box::new(expr),
				rhs: Box::new(rhs),
			})
		}
		_ => Ok(expr)
	}
}


#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EventPrecedence {
	Min,
	Or,
	Iff,
	Max,
}


fn as_edge_ident(tkn: Token) -> EdgeIdent {
	match tkn {
		Keyword(Kw::Edge)    => EdgeIdent::Edge,
		Keyword(Kw::Posedge) => EdgeIdent::Posedge,
		Keyword(Kw::Negedge) => EdgeIdent::Negedge,
		_ => EdgeIdent::Implicit,
	}
}


fn parse_call_args(p: &mut AbstractParser) -> ReportedResult<Vec<CallArg>> {
	let mut v = Vec::new();
	if p.peek(0).0 == CloseDelim(Paren) {
		return Ok(v);
	}
	loop {
		match p.peek(0) {
			(Comma, sp) => v.push(CallArg {
				span: sp,
				name_span: sp,
				name: None,
				expr: None,
			}),
			(Period, mut sp) => {
				p.bump();
				let (name, mut name_sp) = p.eat_ident("argument name")?;
				name_sp.expand(sp);
				let expr = flanked(p, Paren, |p| Ok(
					if p.peek(0).0 == CloseDelim(Paren) {
						None
					} else {
						Some(parse_expr(p)?)
					}
				))?;
				sp.expand(p.last_span());

				v.push(CallArg {
					span: sp,
					name_span: name_sp,
					name: Some(name),
					expr: expr,
				});
			}
			(_, mut sp) => {
				let expr = parse_expr(p)?;
				sp.expand(p.last_span());
				v.push(CallArg {
					span: sp,
					name_span: sp,
					name: None,
					expr: Some(expr),
				});
			}
		}

		match p.peek(0) {
			(Comma, sp) => {
				p.bump();
				if p.try_eat(CloseDelim(Paren)) {
					p.add_diag(DiagBuilder2::warning("Superfluous trailing comma").span(sp));
					break;
				}
			},
			(CloseDelim(Paren), _) => break,
			(_, sp) => {
				p.add_diag(DiagBuilder2::error("Expected , or ) after call argument").span(sp));
				return Err(());
			}
		}
	}
	Ok(v)
}


fn parse_variable_decl_assignment(p: &mut AbstractParser) -> ReportedResult<VarDeclName> {
	let mut span = p.peek(0).1;

	// Parse the variable name.
	let (name, name_span) = p.eat_ident("variable name")?;

	// Parse the optional dimensions.
	let (dims, _) = parse_optional_dimensions(p)?;

	// Parse the optional initial expression.
	let init = if p.try_eat(Operator(Op::Assign)) {
		Some(parse_expr(p)?)
	} else {
		None
	};
	span.expand(p.last_span());

	Ok(VarDeclName {
		span: span,
		name: name,
		name_span: name_span,
		dims: dims,
		init: init,
	})
}


fn parse_genvar_decl(p: &mut AbstractParser) -> ReportedResult<GenvarDecl> {
	let mut span = p.peek(0).1;

	// Parse the genvar name.
	let (name, name_span) = p.eat_ident("genvar name")?;

	// Parse the optional initial expression.
	let init = if p.try_eat(Operator(Op::Assign)) {
		Some(parse_expr(p)?)
	} else {
		None
	};
	span.expand(p.last_span());

	Ok(GenvarDecl {
		span: span,
		name: name,
		name_span: name_span,
		init: init,
	})
}


fn parse_generate_item(p: &mut AbstractParser) -> ReportedResult<()> {
	match p.peek(0).0 {
		Keyword(Kw::For)  => parse_generate_for(p),
		Keyword(Kw::If)   => parse_generate_if(p),
		Keyword(Kw::Case) => parse_generate_case(p),
		_ => parse_hierarchy_item(p).map(|_| ()),
	}
}


fn parse_generate_for(p: &mut AbstractParser) -> ReportedResult<()> {
	p.require_reported(Keyword(Kw::For))?;
	flanked(p, Paren, |p|{
		parse_stmt(p)?;
		parse_expr(p)?;
		p.require_reported(Semicolon)?;
		parse_expr(p)?;
		Ok(())
	})?;
	parse_generate_block(p)?;
	Ok(())
}


fn parse_generate_if(p: &mut AbstractParser) -> ReportedResult<()> {
	p.require_reported(Keyword(Kw::If))?;
	flanked(p, Paren, parse_expr)?;
	parse_generate_block(p)?;
	if p.try_eat(Keyword(Kw::Else)) {
		parse_generate_block(p)?;
	}
	Ok(())
}


fn parse_generate_case(p: &mut AbstractParser) -> ReportedResult<()> {
	p.require_reported(Keyword(Kw::Case))?;
	let q = p.last_span();
	p.add_diag(DiagBuilder2::error("Don't know how to parse case-generate statements").span(q));
	Err(())
}


fn parse_generate_block(p: &mut AbstractParser) -> ReportedResult<()> {
	let mut span = p.peek(0).1;

	// Parse the optional block label.
	let mut label = if p.is_ident() && p.peek(1).0 == Colon {
		let (n, _) = p.eat_ident("generate block label")?;
		p.require_reported(Colon)?;
		Some(n)
	} else {
		None
	};

	// Consume the opening "begin" keyword if present.
	if !p.try_eat(OpenDelim(Bgend)) {
		if label.is_some() {
			let (t,q) = p.peek(0);
			p.add_diag(DiagBuilder2::error(format!("Expected `begin` keyword after generate block label, found {} instead", t)).span(q));
			return Err(());
		}
		parse_generate_item(p)?;
		return Ok(())
	}

	// Consume the optional label after the "begin" keyword.
	if p.try_eat(Colon) {
		let (n, sp) = p.eat_ident("generate block label")?;
		if let Some(existing) = label {
			if existing == n {
				p.add_diag(DiagBuilder2::warning(format!("Generate block {} labelled twice", n)).span(sp));
			} else {
				p.add_diag(DiagBuilder2::error(format!("Generate block given conflicting labels {} and {}", existing, n)).span(sp));
				return Err(());
			}
		} else {
			label = Some(n);
		}
	}

	repeat_until(p, CloseDelim(Bgend), parse_generate_item)?;
	p.require_reported(CloseDelim(Bgend))?;

	// Consume the optional label after the "end" keyword.
	if p.try_eat(Colon) {
		let (n, sp) = p.eat_ident("generate block label")?;
		if let Some(existing) = label {
			if existing != n {
				p.add_diag(DiagBuilder2::error(format!("Label {} given after generate block does not match label {} given before the block", n, existing)).span(sp));
				return Err(());
			}
		} else {
			p.add_diag(DiagBuilder2::warning(format!("Generate block has trailing label {}, but is missing leading label", n)).span(sp));
		}
	}

	span.expand(p.last_span());
	Ok(())
}


fn parse_class_decl(p: &mut AbstractParser) -> ReportedResult<ClassDecl> {
	let mut span = p.peek(0).1;
	let (
		virt,
		lifetime,
		name,
		name_span,
		params,
		extends,
		items
	) = recovered(p, Keyword(Kw::Endclass), |p|{

		// Eat the optional "virtual" keyword.
		let virt = p.try_eat(Keyword(Kw::Virtual));

		// Eat the "class" keyword.
		p.require_reported(Keyword(Kw::Class))?;

		// Eat the optional lifetime.
		let lifetime = match as_lifetime(p.peek(0).0) {
			Some(l) => { p.bump(); l },
			None => Lifetime::Static,
		};

		// Parse the class name.
		let (name, name_span) = p.eat_ident("class name")?;

		// Parse the optional parameter port list.
		let params = if p.try_eat(Hashtag) {
			parse_parameter_port_list(p)?
		} else {
			Vec::new()
		};

		// Parse the optional inheritance clause.
		let extends = if p.try_eat(Keyword(Kw::Extends)) {
			let superclass = parse_data_type(p)?;
			let args = try_flanked(p, Paren, parse_call_args)?.unwrap_or(Vec::new());
			Some((superclass, args))
		} else {
			None
		};
		p.require_reported(Semicolon)?;

		// Parse the class items.
		let items = repeat_until(p, Keyword(Kw::Endclass), parse_class_item)?;
		Ok((virt, lifetime, name, name_span, params, extends, items))
	})?;
	p.require_reported(Keyword(Kw::Endclass))?;

	// Parse the optional class name after "endclass".
	if p.try_eat(Colon) {
		let (n, sp) = p.eat_ident("class name")?;
		if n != name {
			p.add_diag(DiagBuilder2::error(format!("Class name {} disagrees with name {} given before", n, name)).span(sp));
			return Err(());
		}
	}

	span.expand(p.last_span());
	Ok(ClassDecl {
		span: span,
		virt: virt,
		lifetime: lifetime,
		name: name,
		name_span: name_span,
		params: params,
		extends: extends,
		items: items,
	})
}


fn parse_class_item(p: &mut AbstractParser) -> ReportedResult<ClassItem> {
	let mut span = p.peek(0).1;

	// Easy path for null class items.
	if p.try_eat(Semicolon) {
		return Ok(ClassItem {
			span: span,
			qualifiers: Vec::new(),
			data: ClassItemData::Null,
		});
	}

	// Parse localparam and parameter declarations.
	match p.peek(0).0 {
		Keyword(Kw::Localparam) => return Ok(ClassItem {
			span: span,
			qualifiers: Vec::new(),
			data: ClassItemData::LocalParamDecl(parse_localparam_decl(p)?),
		}),
		Keyword(Kw::Parameter) => return Ok(ClassItem {
			span: span,
			qualifiers: Vec::new(),
			data: ClassItemData::ParameterDecl(parse_parameter_decl(p)?),
		}),
		_ => ()
	}

	// Parse "extern" task and function prototypes.
	if p.try_eat(Keyword(Kw::Extern)) {
		let proto = parse_subroutine_prototype(p)?;
		span.expand(p.last_span());
		return Ok(ClassItem {
			span: span,
			qualifiers: Vec::new(),
			data: ClassItemData::ExternSubroutine(proto),
		})
	}

	// Parse the optional class item qualifiers.
	let qualifiers = parse_class_item_qualifiers(p)?;

	let data = {
		let mut pp = ParallelParser::new();
		pp.add("class property", |p| {
			let ty = parse_data_type(p)?;
			let names = comma_list_nonempty(p, Semicolon, "data declaration", parse_variable_decl_assignment)?;
			p.require_reported(Semicolon)?;
			Ok(ClassItemData::Property)
		});
		pp.add("class function or task", |p| parse_subroutine_decl(p).map(|d| ClassItemData::SubroutineDecl(d)));
		pp.add("class constraint", |p| parse_constraint(p).map(|c| ClassItemData::Constraint(c)));
		pp.finish(p, "class item")?
	};
	span.expand(p.last_span());

	Ok(ClassItem {
		span: span,
		qualifiers: qualifiers,
		data: data,
	})
}


fn parse_class_item_qualifiers(p: &mut AbstractParser) -> ReportedResult<Vec<(ClassItemQualifier,Span)>> {
	let mut v = Vec::new();
	loop {
		let (tkn,sp) = p.peek(0);
		match tkn {
			Keyword(Kw::Static)    => v.push((ClassItemQualifier::Static, sp)),
			Keyword(Kw::Protected) => v.push((ClassItemQualifier::Protected, sp)),
			Keyword(Kw::Local)     => v.push((ClassItemQualifier::Local, sp)),
			Keyword(Kw::Rand)      => v.push((ClassItemQualifier::Rand, sp)),
			Keyword(Kw::Randc)     => v.push((ClassItemQualifier::Randc, sp)),
			Keyword(Kw::Pure)      => v.push((ClassItemQualifier::Pure, sp)),
			Keyword(Kw::Virtual)   => v.push((ClassItemQualifier::Virtual, sp)),
			Keyword(Kw::Const)     => v.push((ClassItemQualifier::Const, sp)),
			_ => break,
		}
		p.bump();
	}
	Ok(v)
}


fn parse_class_method(p: &mut AbstractParser) -> ReportedResult<ClassItem> {
	println!("Parsing class method");
	Err(())
}


fn parse_class_property(p: &mut AbstractParser) -> ReportedResult<ClassItem> {
	println!("Parsing class property");
	p.try_eat(Keyword(Kw::Rand));
	Err(())
}


fn parse_constraint(p: &mut AbstractParser) -> ReportedResult<Constraint> {
	let mut span = p.peek(0).1;

	// Parse the prototype qualifier.
	let kind = match p.peek(0).0 {
		Keyword(Kw::Extern) => { p.bump(); ConstraintKind::ExternProto },
		Keyword(Kw::Pure) => { p.bump(); ConstraintKind::PureProto },
		_ => ConstraintKind::Decl,
	};
	let kind_span = span;

	// Parse the optional "static" keyword.
	let statik = p.try_eat(Keyword(Kw::Static));

	// Parse the "constraint" keyword.
	p.require_reported(Keyword(Kw::Constraint))?;

	// Parse the constraint name.
	let (name, name_span) = p.eat_ident("constraint name")?;

	let items = if p.try_eat(Semicolon) {
		let kind = match kind {
			ConstraintKind::Decl => ConstraintKind::Proto,
			x => x,
		};
		Vec::new()
	} else {
		// Make sure that no "extern" or "pure" keyword was used, as these are
		// only valid for prototypes.
		if kind == ConstraintKind::ExternProto || kind == ConstraintKind::PureProto {
			p.add_diag(DiagBuilder2::error("Only constraint prototypes can be extern or pure").span(kind_span));
			return Err(());
		}
		flanked(p, Brace, |p| repeat_until(p, CloseDelim(Brace), parse_constraint_item))?
	};
	span.expand(p.last_span());

	Ok(Constraint {
		span: span,
		kind: kind,
		statik: statik,
		name: name,
		name_span: name_span,
		items: items,
	})
}


fn parse_constraint_item(p: &mut AbstractParser) -> ReportedResult<ConstraintItem> {
	let mut span = p.peek(0).1;
	let data = parse_constraint_item_data(p)?;
	span.expand(p.last_span());
	Ok(ConstraintItem {
		span: span,
		data: data,
	})
}


fn parse_constraint_item_data(p: &mut AbstractParser) -> ReportedResult<ConstraintItemData> {
	// Handle the trivial cases that start with a keyword first.
	if p.try_eat(Keyword(Kw::If)) {
		let q = p.last_span();
		p.add_diag(DiagBuilder2::error("Don't know how to parse `if` constraint items").span(q));
		return Err(());
	}

	if p.try_eat(Keyword(Kw::Foreach)) {
		let q = p.last_span();
		p.add_diag(DiagBuilder2::error("Don't know how to parse `foreach` constraint items").span(q));
		return Err(());
	}

	// If we arrive here, the item starts with an expression.
	let expr = parse_expr(p)?;
	p.require_reported(Semicolon)?;
	Ok(ConstraintItemData::Expr(expr))
}


struct ParallelParser<R: Clone> {
	branches: Vec<(String, Box<FnMut(&mut AbstractParser) -> ReportedResult<R>>, bool)>,
}

impl<R: Clone> ParallelParser<R> {
	pub fn new() -> Self {
		ParallelParser {
			branches: Vec::new(),
		}
	}

	pub fn add<F>(&mut self, name: &str, func: F)
	where F: FnMut(&mut AbstractParser) -> ReportedResult<R> + 'static {
		self.branches.push((name.to_owned(), Box::new(func), false));
	}

	pub fn add_greedy<F>(&mut self, name: &str, func: F)
	where F: FnMut(&mut AbstractParser) -> ReportedResult<R> + 'static {
		self.branches.push((name.to_owned(), Box::new(func), true));
	}

	pub fn finish(self, p: &mut AbstractParser, msg: &str) -> ReportedResult<R> {
		let (tkn, q) = p.peek(0);
		// p.add_diag(DiagBuilder2::note(format!("Trying as {:?}", self.branches.iter().map(|&(ref x,_)| x).collect::<Vec<_>>())).span(q));

		// Create a separate speculative parser for each branch.
		let mut results = Vec::new();
		let mut matched = Vec::new();
		for (name, mut func, greedy) in self.branches {
			// p.add_diag(DiagBuilder2::note(format!("Trying as {}", name)).span(q));
			let mut bp = BranchParser::new(p);
			match func(&mut bp) {
				Ok(x) => {
					if greedy {
						bp.commit();
						return Ok(x);
					} else {
						let sp = bp.last_span();
						results.push((name, bp.consumed, bp.diagnostics, x, Span::union(q, sp)));
					}
				}
				Err(_) => matched.push((name, bp.consumed() - bp.skipped(), bp.diagnostics)),
			}
		}

		if results.len() > 1 {
			let mut names = String::new();
			names.push_str(&results[0].0);
			if results.len() == 2 {
				names.push_str(" or ");
				names.push_str(&results[1].0);
			} else {
				for &(ref name, _, _, _, _) in &results[..results.len()-1] {
					names.push_str(", ");
					names.push_str(&name);
				}
				names.push_str(", or ");
				names.push_str(&results[results.len()-1].0);
			}
			p.add_diag(DiagBuilder2::fatal(format!("Ambiguous code, could be {}", names)).span(q));
			for &(ref name, _, _, _, span) in &results {
				p.add_diag(DiagBuilder2::note(format!("{} would be this part", name)).span(span));
			}
			Err(())
		} else if let Some(&(_, consumed, ref diagnostics, ref res, _)) = results.last() {
			for d in diagnostics {
				p.add_diag(d.clone());
			}
			for _ in 0..consumed {
				p.bump();
			}
			Ok((*res).clone())
		} else {
			// Sort the errors by score and remove all but the highest scoring
			// ones.
			matched.sort_by(|a,b| (b.1).cmp(&a.1));
			let highest_score = matched[0].1;
			let errors = matched.into_iter().take_while(|e| e.1 == highest_score).collect::<Vec<_>>();
			let num_errors = errors.len();

			// Print the errors.
			if num_errors != 1 {
				p.add_diag(DiagBuilder2::error(format!("Expected {}, found {} instead", msg, tkn)).span(q));
			} else {
				for d in errors.into_iter().next().unwrap().2 {
					p.add_diag(d);
				}
			}
			Err(())

			// if errors.is_empty() {
			// 	Err(())
			// } else {
			// 	for (n, _, m) in errors {
			// 		if num_errors > 1 {
			// 			p.add_diag(DiagBuilder2::note(format!("Assuming this is a {}", n)).span(q));
			// 		}
			// 		for d in m {
			// 			p.add_diag(d);
			// 		}
			// 	}
			// 	Err(())
			// }
		}
	}
}

struct BranchParser<'tp> {
	parser: &'tp mut AbstractParser,
	consumed: usize,
	skipped: usize,
	diagnostics: Vec<DiagBuilder2>,
	last_span: Span,
	severity: Severity,
}

impl<'tp> BranchParser<'tp> {
	pub fn new(parser: &'tp mut AbstractParser) -> Self {
		let last = parser.last_span();
		BranchParser {
			parser: parser,
			consumed: 0,
			skipped: 0,
			diagnostics: Vec::new(),
			last_span: last,
			severity: Severity::Note,
		}
	}

	pub fn skipped(&self) -> usize {
		self.skipped
	}

	pub fn commit(self) {
		for _ in 0..self.consumed {
			self.parser.bump();
		}
		for d in self.diagnostics {
			self.parser.add_diag(d);
		}
	}
}

impl<'tp> AbstractParser for BranchParser<'tp> {
	fn peek(&mut self, offset: usize) -> TokenAndSpan {
		self.parser.peek(self.consumed + offset)
	}

	fn bump(&mut self) {
		self.last_span = self.parser.peek(self.consumed).1;
		self.consumed += 1;
	}

	fn skip(&mut self) {
		self.bump();
		self.skipped += 1;
	}

	fn consumed(&self) -> usize {
		self.consumed
	}

	fn last_span(&self) -> Span {
		self.last_span
	}

	fn add_diag(&mut self, diag: DiagBuilder2) {
		if diag.severity > self.severity {
			self.severity = diag.severity;
		}
		self.diagnostics.push(diag);
	}

	fn severity(&self) -> Severity {
		self.severity
	}
}


fn parse_typedef(p: &mut AbstractParser) -> ReportedResult<Typedef> {
	p.bump();
	let mut span = p.last_span();
	let ty = parse_data_type(p)?;
	let (name, name_span) = p.eat_ident("type name")?;
	let (dims, _) = parse_optional_dimensions(p)?;
	p.require_reported(Semicolon)?;
	span.expand(p.last_span());
	Ok(Typedef {
		span: span,
		name: name,
		name_span: name_span,
		ty: ty,
		dims: dims,
	})
}


fn parse_port_decl(p: &mut AbstractParser) -> ReportedResult<PortDecl> {
	let mut span = p.peek(0).1;

	// Consume the port direction.
	let dir = match as_port_direction(p.peek(0).0) {
		Some(x) => { p.bump(); x },
		None => {
			p.add_diag(DiagBuilder2::error("Expected port direction (inout, input, output, or ref)").span(span));
			return Err(());
		}
	};

	// Consume the optional net type or "var" keyword.
	let net_type = as_net_type(p.peek(0).0);
	let var = if net_type.is_some() {
		p.bump();
		false
	} else {
		p.try_eat(Keyword(Kw::Var))
	};

	// Branch to handle explicit and implicit types.
	let mut pp = ParallelParser::new();
	pp.add("explicit type", |p|{
		let ty = parse_explicit_type(p)?;
		Ok((ty, tail(p)?))
	});
	pp.add("implicit type", |p|{
		let ty = parse_implicit_type(p)?;
		Ok((ty, tail(p)?))
	});
	let (ty, names) = pp.finish(p, "explicit or implicit type")?;

	fn tail(p: &mut AbstractParser) -> ReportedResult<Vec<VarDeclName>> {
		let names = comma_list_nonempty(p, Semicolon, "port declaration", parse_variable_decl_assignment)?;
		p.require_reported(Semicolon)?;
		Ok(names)
	}

	// Wrap things up.
	span.expand(p.last_span());
	Ok(PortDecl {
		span: span,
		dir: dir,
		net_type: net_type,
		var: var,
		ty: ty,
		names: names,
	})
}


fn as_net_type(tkn: Token) -> Option<NetType> {
	match tkn {
		Keyword(Kw::Supply0) => Some(NetType::Supply0),
		Keyword(Kw::Supply1) => Some(NetType::Supply1),
		Keyword(Kw::Tri)     => Some(NetType::Tri),
		Keyword(Kw::Triand)  => Some(NetType::TriAnd),
		Keyword(Kw::Trior)   => Some(NetType::TriOr),
		Keyword(Kw::Trireg)  => Some(NetType::TriReg),
		Keyword(Kw::Tri0)    => Some(NetType::Tri0),
		Keyword(Kw::Tri1)    => Some(NetType::Tri1),
		Keyword(Kw::Uwire)   => Some(NetType::Uwire),
		Keyword(Kw::Wire)    => Some(NetType::Wire),
		Keyword(Kw::Wand)    => Some(NetType::WireAnd),
		Keyword(Kw::Wor)     => Some(NetType::WireOr),
		_ => None
	}
}


fn parse_net_decl(p: &mut AbstractParser) -> ReportedResult<NetDecl> {
	let mut span = p.peek(0).1;

	// Consume the net type.
	let net_type = match as_net_type(p.peek(0).0) {
		Some(x) => { p.bump(); x },
		None => {
			let q = p.peek(0).1;
			p.add_diag(DiagBuilder2::error("Expected net type").span(q));
			return Err(());
		}
	};

	// Consume the optional drive strength or charge strength.
	let strength = try_flanked(p, Paren, parse_net_strength)?;

	// Consume the optional "vectored" or "scalared" keywords.
	let kind = match p.peek(0).0 {
		Keyword(Kw::Vectored) => { p.bump(); NetKind::Vectored },
		Keyword(Kw::Scalared) => { p.bump(); NetKind::Scalared },
		_ => NetKind::None
	};

	// Branch to handle explicit and implicit types separately.
	let mut pp = ParallelParser::new();
	pp.add("explicit type", |p|{
		let ty = parse_explicit_type(p)?;
		Ok((ty, tail(p)?))
	});
	pp.add("implicit type", |p|{
		let ty = parse_implicit_type(p)?;
		Ok((ty, tail(p)?))
	});
	let (ty, (delay, names)) = pp.finish(p, "explicit or implicit type")?;

	// This function handles parsing of everything after the type.
	fn tail(p: &mut AbstractParser) -> ReportedResult<(Option<Expr>, Vec<VarDeclName>)> {
		// Parse the optional delay.
		let delay = if p.try_eat(Hashtag) {
			let q = p.last_span();
			p.add_diag(DiagBuilder2::error("Don't know how to parse delays on net declarations").span(q));
			return Err(());
		} else {
			None
		};

		// Parse the names and assignments.
		let names = comma_list_nonempty(p, Semicolon, "net declaration", parse_variable_decl_assignment)?;
		p.require_reported(Semicolon)?;
		Ok((delay, names))
	}

	span.expand(p.last_span());
	Ok(NetDecl {
		span: span,
		net_type: net_type,
		strength: strength,
		kind: kind,
		ty: ty,
		delay: delay,
		names: names,
	})
}


fn parse_net_strength(p: &mut AbstractParser) -> ReportedResult<NetStrength> {
	if let Some(a) = as_drive_strength(p.peek(0).0) {
		p.bump();
		p.require_reported(Comma)?;
		if let Some(b) = as_drive_strength(p.peek(0).0) {
			Ok(NetStrength::Drive(a,b))
		} else {
			let q = p.peek(0).1;
			p.add_diag(DiagBuilder2::error("Expected second drive strength").span(q));
			Err(())
		}
	} else if let Some(s) = as_charge_strength(p.peek(0).0) {
		p.bump();
		Ok(NetStrength::Charge(s))
	} else {
		let q = p.peek(0).1;
		p.add_diag(DiagBuilder2::error("Expected drive or charge strength").span(q));
		Err(())
	}
}


fn as_drive_strength(tkn: Token) -> Option<DriveStrength> {
	match tkn {
		Keyword(Kw::Supply0) => Some(DriveStrength::Supply0),
		Keyword(Kw::Strong0) => Some(DriveStrength::Strong0),
		Keyword(Kw::Pull0)   => Some(DriveStrength::Pull0),
		Keyword(Kw::Weak0)   => Some(DriveStrength::Weak0),
		Keyword(Kw::Highz0)  => Some(DriveStrength::HighZ0),
		Keyword(Kw::Supply1) => Some(DriveStrength::Supply1),
		Keyword(Kw::Strong1) => Some(DriveStrength::Strong1),
		Keyword(Kw::Pull1)   => Some(DriveStrength::Pull1),
		Keyword(Kw::Weak1)   => Some(DriveStrength::Weak1),
		Keyword(Kw::Highz1)  => Some(DriveStrength::HighZ1),
		_ => None
	}
}


fn as_charge_strength(tkn: Token) -> Option<ChargeStrength> {
	match tkn {
		Keyword(Kw::Small)  => Some(ChargeStrength::Small),
		Keyword(Kw::Medium) => Some(ChargeStrength::Medium),
		Keyword(Kw::Large)  => Some(ChargeStrength::Large),
		_ => None
	}
}


/// Parse a import declaration.
/// ```
/// "import" package_ident "::" "*" ";"
/// "import" package_ident "::" ident ";"
/// ```
fn parse_import_decl(p: &mut AbstractParser) -> ReportedResult<ImportDecl> {
	let mut span = p.peek(0).1;
	p.require_reported(Keyword(Kw::Import))?;
	let items = comma_list_nonempty(p, Semicolon, "import item", |p|{
		// package_ident "::" ident
		// package_ident "::" "*"
		let (pkg, pkg_span) = p.eat_ident("package name")?;
		p.require_reported(Namespace)?;
		let (tkn, sp) = p.peek(0);
		match tkn {
			// package_ident "::" "*"
			Operator(Op::Mul) => {
				p.bump();
				Ok(ImportItem {
					pkg: pkg,
					pkg_span: pkg_span,
					name: None,
					name_span: sp,
				})
			}

			// package_ident "::" ident
			Ident(n) | EscIdent(n) => {
				p.bump();
				Ok(ImportItem {
					pkg: pkg,
					pkg_span: pkg_span,
					name: Some(n),
					name_span: sp,
				})
			}

			_ => {
				p.add_diag(DiagBuilder2::error("Expected identifier or * after :: in import declaration").span(sp));
				Err(())
			}
		}
	})?;
	p.require_reported(Semicolon)?;
	span.expand(p.last_span());
	Ok(ImportDecl {
		span: span,
		items: items,
	})
}


fn parse_assertion(p: &mut AbstractParser) -> ReportedResult<Assertion> {
	let mut span = p.peek(0).1;

	// Peek ahead after the current token to see if a "property", "sequence", or
	// "#0" follows. This decides what kind of assertion we're parsing.
	let null = get_name_table().intern("0", false);
	let is_property = p.peek(1).0 == Keyword(Kw::Property);
	let is_sequence = p.peek(1).0 == Keyword(Kw::Sequence);
	let is_deferred = p.peek(1).0 == Hashtag && p.peek(2).0 == UnsignedNumber(null);

	// Handle the different combinations of keywords and lookaheads from above.

	let data = match p.peek(0).0 {

		// Concurrent Assertions
		// ---------------------

		// `assert property`
		Keyword(Kw::Assert) if is_property => {
			p.bump();
			p.bump();
			let prop = flanked(p, Paren, parse_property_spec)?;
			let action = parse_assertion_action_block(p)?;
			AssertionData::Concurrent(ConcurrentAssertion::AssertProperty(prop, action))
		}

		// `assume property`
		Keyword(Kw::Assume) if is_property => {
			p.bump();
			p.bump();
			let prop = flanked(p, Paren, parse_property_spec)?;
			let action = parse_assertion_action_block(p)?;
			AssertionData::Concurrent(ConcurrentAssertion::AssumeProperty(prop, action))
		}

		// `cover property`
		Keyword(Kw::Cover) if is_property => {
			p.bump();
			p.bump();
			let prop = flanked(p, Paren, parse_property_spec)?;
			let stmt = parse_stmt(p)?;
			AssertionData::Concurrent(ConcurrentAssertion::CoverProperty(prop, stmt))
		}

		// `cover sequence`
		Keyword(Kw::Cover) if is_sequence => {
			p.bump();
			p.bump();
			p.add_diag(DiagBuilder2::error("Don't know how to parse cover sequences").span(span));
			return Err(());
			// AssertionData::Concurrent(ConcurrentAssertion::CoverSequence)
		}

		// `expect`
		Keyword(Kw::Expect) => {
			p.bump();
			let prop = flanked(p, Paren, parse_property_spec)?;
			let action = parse_assertion_action_block(p)?;
			AssertionData::Concurrent(ConcurrentAssertion::ExpectProperty(prop, action))
		}

		// `restrict property`
		Keyword(Kw::Restrict) if is_property => {
			p.bump();
			p.bump();
			let prop = flanked(p, Paren, parse_property_spec)?;
			AssertionData::Concurrent(ConcurrentAssertion::RestrictProperty(prop))
		}

		// Immediate and Deferred Assertions
		// ---------------------------------

		// `assert` and `assert #0`
		Keyword(Kw::Assert) => {
			p.bump();
			if is_deferred {
				p.bump();
				p.bump();
			}
			let expr = flanked(p, Paren, parse_expr)?;
			let action = parse_assertion_action_block(p)?;
			let a = BlockingAssertion::Assert(expr, action);
			if is_deferred {
				AssertionData::Deferred(a)
			} else {
				AssertionData::Immediate(a)
			}
		}

		// `assume` and `assume #0`
		Keyword(Kw::Assume) => {
			p.bump();
			if is_deferred {
				p.bump();
				p.bump();
			}
			let expr = flanked(p, Paren, parse_expr)?;
			let action = parse_assertion_action_block(p)?;
			let a = BlockingAssertion::Assume(expr, action);
			if is_deferred {
				AssertionData::Deferred(a)
			} else {
				AssertionData::Immediate(a)
			}
		}

		// `cover` and `cover #0`
		Keyword(Kw::Cover) => {
			p.bump();
			if is_deferred {
				p.bump();
				p.bump();
			}
			let expr = flanked(p, Paren, parse_expr)?;
			let stmt = parse_stmt(p)?;
			let a = BlockingAssertion::Cover(expr, stmt);
			if is_deferred {
				AssertionData::Deferred(a)
			} else {
				AssertionData::Immediate(a)
			}
		}

		_ => {
			p.add_diag(DiagBuilder2::error("Expected assert, assume, cover, expect, or restrict").span(span));
			return Err(());
		}
	};

	span.expand(p.last_span());
	Ok(Assertion {
		span: span,
		label: None,
		data: data,
	})
}


fn parse_assertion_action_block(p: &mut AbstractParser) -> ReportedResult<AssertionActionBlock> {
	if p.try_eat(Keyword(Kw::Else)) {
		Ok(AssertionActionBlock::Negative(parse_stmt(p)?))
	} else {
		let stmt = parse_stmt(p)?;
		if p.try_eat(Keyword(Kw::Else)) {
			// TODO: Ensure that `stmt` is not a NullStmt.
			Ok(AssertionActionBlock::Both(stmt, parse_stmt(p)?))
		} else {
			Ok(AssertionActionBlock::Positive(stmt))
		}
	}
}


fn parse_property_spec(p: &mut AbstractParser) -> ReportedResult<PropSpec> {
	let mut span = p.peek(0).1;

	// Parse the optional event expression.
	let event = if p.try_eat(At) {
		Some(parse_event_expr(p, EventPrecedence::Min)?)
	} else {
		None
	};

	// Parse the optional "disable iff" clause.
	let disable = if p.try_eat(Keyword(Kw::Disable)) {
		p.require_reported(Keyword(Kw::Iff))?;
		Some(flanked(p, Paren, parse_expr)?)
	} else {
		None
	};

	// Parse the property expression.
	let prop = parse_propexpr(p)?;
	Ok(PropSpec)
}



#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PropSeqPrecedence {
	Min,
	AlEvIfAccRejSyn,
	ImplFollow, // right-associative
	Until,      // right-associative
	Iff,        // right-associative
	Or,         // left-associative
	And,        // left-associative
	NotNexttime,
	Intersect,  // left-associative
	Within,     // left-associative
	Throughout, // right-associative
	CycleDelay, // left-associative
	Brack,
	Max,
}


fn parse_propexpr(p: &mut AbstractParser) -> ReportedResult<PropExpr> {
	parse_propexpr_prec(p, PropSeqPrecedence::Min)
}


fn parse_propexpr_prec(p: &mut AbstractParser, precedence: PropSeqPrecedence) -> ReportedResult<PropExpr> {
	let mut span = p.peek(0).1;

	// To parse property expressions we need a parallel parser. For certain
	// cases it is unclear if a parenthesized expression is a sequence or a
	// property expression, e.g.:
	//
	// (foo) |=> bar
	// ^^^^^ sequence or property?
	//
	// Both sequences and property expressions support parenthesis. However the
	// |=> operator is only defined for sequences on the left hand side. If the
	// parenthesis are parsed as a property and foo as a sequence, the above
	// code fails to parse since the sequence on the left has effectively become
	// a property. If the parenthesis are parsed as a sequence, all is well. To
	// resolve these kinds of issues, we need a parallel parser.
	let mut pp = ParallelParser::new();
	pp.add_greedy("sequence expression", move |p| parse_propexpr_seq(p, precedence));
	pp.add_greedy("property expression", move |p| parse_propexpr_nonseq(p, precedence));
	let data = pp.finish(p, "sequence or primary property expression")?;

	span.expand(p.last_span());
	let expr = PropExpr {
		span: span,
		data: data,
	};
	parse_propexpr_suffix(p, expr, precedence)
}


fn parse_propexpr_nonseq(p: &mut AbstractParser, precedence: PropSeqPrecedence) -> ReportedResult<PropExprData> {
	// Handle the trivial case of expressions introduced by a symbol or keyword.
	match p.peek(0).0 {
		// Parenthesized property expression.
		OpenDelim(Paren) => return flanked(p, Paren, parse_propexpr).map(|pe| pe.data),

		// "not" operator
		Keyword(Kw::Not) => {
			p.bump();
			let expr = parse_propexpr_prec(p, PropSeqPrecedence::NotNexttime)?;
			return Ok(PropExprData::Not(Box::new(expr)));
		}

		// Clocking event
		At => {
			p.bump();
			let ev = parse_event_expr(p, EventPrecedence::Min)?;
			let expr = parse_propexpr(p)?;
			return Ok(PropExprData::Clocked(ev, Box::new(expr)));
		}

		_ => {
			let q = p.peek(0).1;
			p.add_diag(DiagBuilder2::error("Expected primary property expression").span(q));
			return Err(());
		}
	}
}


fn parse_propexpr_seq(p: &mut AbstractParser, precedence: PropSeqPrecedence) -> ReportedResult<PropExprData> {
	// Consume a strong, weak, or regular sequence operator.
	let (seqop, seqexpr) = match p.peek(0).0 {
		Keyword(Kw::Strong) => {
			p.bump();
			(PropSeqOp::Strong, flanked(p, Paren, parse_seqexpr)?)
		}
		Keyword(Kw::Weak) => {
			p.bump();
			(PropSeqOp::Weak, flanked(p, Paren, parse_seqexpr)?)
		}
		_ => (PropSeqOp::None, parse_seqexpr_prec(p, precedence)?)
	};

	// Handle the operators that have a sequence expression on their left hand
	// side.
	if precedence <= PropSeqPrecedence::ImplFollow {
		if let Some(op) = match p.peek(0).0 {
			Operator(Op::SeqImplOl)    => Some(PropSeqBinOp::ImplOverlap),
			Operator(Op::SeqImplNol)   => Some(PropSeqBinOp::ImplNonoverlap),
			Operator(Op::SeqFollowOl)  => Some(PropSeqBinOp::FollowOverlap),
			Operator(Op::SeqFollowNol) => Some(PropSeqBinOp::FollowNonoverlap),
			_ => None
		}{
			p.bump();
			let expr = parse_propexpr_prec(p, PropSeqPrecedence::ImplFollow)?;
			return Ok(PropExprData::SeqBinOp(op, seqop, seqexpr, Box::new(expr)));
		}
	}

	// Otherwise this is just a simple sequence operator.
	Ok(PropExprData::SeqOp(seqop, seqexpr))
}


fn parse_propexpr_suffix(p: &mut AbstractParser, prefix: PropExpr, precedence: PropSeqPrecedence) -> ReportedResult<PropExpr> {

	// Handle the binary operators that have a property expression on both their
	// left and right hand side.
	if let Some((op, prec, rassoc)) = match p.peek(0).0 {
		Keyword(Kw::Or)         => Some((PropBinOp::Or,         PropSeqPrecedence::Or,    false)),
		Keyword(Kw::And)        => Some((PropBinOp::And,        PropSeqPrecedence::And,   false)),
		Keyword(Kw::Until)      => Some((PropBinOp::Until,      PropSeqPrecedence::Until, true)),
		Keyword(Kw::SUntil)     => Some((PropBinOp::SUntil,     PropSeqPrecedence::Until, true)),
		Keyword(Kw::UntilWith)  => Some((PropBinOp::UntilWith,  PropSeqPrecedence::Until, true)),
		Keyword(Kw::SUntilWith) => Some((PropBinOp::SUntilWith, PropSeqPrecedence::Until, true)),
		Keyword(Kw::Implies)    => Some((PropBinOp::Impl,       PropSeqPrecedence::Until, true)),
		Keyword(Kw::Iff)        => Some((PropBinOp::Iff,        PropSeqPrecedence::Iff,   true)),
		_ => None
	}{
		if precedence < prec || (rassoc && precedence == prec) {
			p.bump();
			let rhs = parse_propexpr_prec(p, prec)?;
			return Ok(PropExpr {
				span: Span::union(prefix.span, rhs.span),
				data: PropExprData::BinOp(op, Box::new(prefix), Box::new(rhs))
			});
		}
	}

	Ok(prefix)
}


fn parse_seqexpr(p: &mut AbstractParser) -> ReportedResult<SeqExpr> {
	parse_seqexpr_prec(p, PropSeqPrecedence::Min)
}


fn parse_seqexpr_prec(p: &mut AbstractParser, precedence: PropSeqPrecedence) -> ReportedResult<SeqExpr> {
	let mut span = p.peek(0).1;

	// See parse_propexpr_prec for an explanation of why we need a parallel
	// parser here.
	let mut pp = ParallelParser::new();
	pp.add_greedy("expression", move |p| parse_seqexpr_expr(p, precedence));
	pp.add_greedy("sequence", move |p| parse_seqexpr_nonexpr(p, precedence));
	let data = pp.finish(p, "sequence or primary property expression")?;

	span.expand(p.last_span());
	let expr = SeqExpr {
		span: span,
		data: data,
	};
	parse_seqexpr_suffix(p, expr, precedence)
}


fn parse_seqexpr_expr(p: &mut AbstractParser, precedence: PropSeqPrecedence) -> ReportedResult<SeqExprData> {
	// TODO: Handle all the non-trivial cases.
	let q = p.peek(0).1;
	p.add_diag(DiagBuilder2::error("Don't know how to parse sequence expression that don't start with an expression").span(q));
	Err(())
}


fn parse_seqexpr_nonexpr(p: &mut AbstractParser, precedence: PropSeqPrecedence) -> ReportedResult<SeqExprData> {

	// If we arrive here, the only possibility left is that this sequence starts
	// with and expression or distribution.
	let expr = parse_expr(p)?;

	// Handle the case of the "throughout" operator that has an expression on
	// its left hand side.
	if precedence <= PropSeqPrecedence::Throughout && p.try_eat(Keyword(Kw::Throughout)) {
		let rhs = parse_seqexpr_prec(p, PropSeqPrecedence::Throughout)?;
		return Ok(SeqExprData::Throughout(expr, Box::new(rhs)));
	}

	// Parse the optional repetition.
	let rep = try_flanked(p, Brack, parse_seqrep)?;

	Ok(SeqExprData::Expr(expr, rep))
}


fn parse_seqexpr_suffix(p: &mut AbstractParser, prefix: SeqExpr, precedence: PropSeqPrecedence) -> ReportedResult<SeqExpr> {
	// TODO: Handle all the binary operators.
	Ok(prefix)
}


fn parse_seqrep(p: &mut AbstractParser) -> ReportedResult<SeqRep> {
	match p.peek(0).0 {
		// [*]
		// [* expr]
		Operator(Op::Mul) => {
			p.bump();
			if p.peek(0).0 == CloseDelim(Brack) {
				Ok(SeqRep::ConsecStar)
			} else {
				Ok(SeqRep::Consec(parse_expr(p)?))
			}
		}

		// [+]
		Operator(Op::Add) => {
			p.bump();
			Ok(SeqRep::ConsecPlus)
		}

		// [= expr]
		Operator(Op::Assign) => {
			p.bump();
			Ok(SeqRep::Nonconsec(parse_expr(p)?))
		}

		// [-> expr]
		Operator(Op::LogicImpl) => {
			p.bump();
			Ok(SeqRep::Goto(parse_expr(p)?))
		}

		_ => {
			let q = p.peek(0).1;
			p.add_diag(DiagBuilder2::error("Expected sequence repetition [+], [*], [* <expr>], [= <expr>], or [-> <expr>]").span(q));
			Err(())
		}
	}
}



#[cfg(test)]
mod tests {
	use source::*;
	use name::*;
	use svlog::preproc::*;
	use svlog::lexer::*;

	fn parse(input: &str) {
		use std::cell::Cell;
		thread_local!(static INDEX: Cell<usize> = Cell::new(0));
		let sm = get_source_manager();
		let idx = INDEX.with(|i| {
			let v = i.get();
			i.set(v+1);
			v
		});
		let source = sm.add(&format!("test_{}.sv", idx), input);
		let pp = Preprocessor::new(source, &[]);
		let lexer = Lexer::new(pp);
		super::parse(lexer);
	}

	#[test]
	fn intf_empty() {
		parse("interface Foo; endinterface");
	}

	#[test]
	fn intf_params() {
		parse("interface Foo #(); endinterface");
		parse("interface Foo #(parameter bar = 32); endinterface");
		parse("interface Foo #(parameter bar = 32, baz = 64); endinterface");
		parse("interface Foo #(parameter bar = 32, parameter baz = 64); endinterface");
	}

	#[test]
	fn intf_header() {
		// parse("interface Foo ();")
	}
}
