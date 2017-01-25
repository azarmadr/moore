// Copyright (c) 2016-2017 Fabian Schuiki

//! Utilities to implement diagnostics and error reporting facilities.

use source::Span;
use std::fmt;



/// A handler deals with errors.
#[derive(Debug)]
pub struct Handler {
}


pub static DUMMY_HANDLER: Handler = Handler{};


/// Used to emit structured error messages.
#[must_use]
#[derive(Clone, Debug)]
pub struct DiagnosticBuilder<'a> {
	pub handler: &'a Handler,
	pub message: String,
}

/// A diagnostic result type. Either carries the result `T` in the Ok variant,
/// or an assembled diagnostic in the Err variant.
pub type DiagResult<'a, T> = Result<T, DiagnosticBuilder<'a>>;



#[must_use]
#[derive(Clone, Debug)]
pub struct DiagBuilder2 {
	pub severity: Severity,
	pub message: String,
	pub span: Option<Span>,
}

/// A diagnostic result type. Either carries the result `T` in the Ok variant,
/// or an assembled diagnostic in the Err variant.
pub type DiagResult2<T> = Result<T, DiagBuilder2>;

impl DiagBuilder2 {
	pub fn fatal<S: Into<String>>(message: S) -> DiagBuilder2 {
		DiagBuilder2 {
			severity: Severity::Fatal,
			message: message.into(),
			span: None,
		}
	}

	pub fn error<S: Into<String>>(message: S) -> DiagBuilder2 {
		DiagBuilder2 {
			severity: Severity::Error,
			message: message.into(),
			span: None,
		}
	}

	pub fn warning<S: Into<String>>(message: S) -> DiagBuilder2 {
		DiagBuilder2 {
			severity: Severity::Warning,
			message: message.into(),
			span: None,
		}
	}

	pub fn note<S: Into<String>>(message: S) -> DiagBuilder2 {
		DiagBuilder2 {
			severity: Severity::Note,
			message: message.into(),
			span: None,
		}
	}

	pub fn span<S: Into<Span>>(self, span: S) -> DiagBuilder2 {
		DiagBuilder2 {
			span: Some(span.into()),
			..self
		}
	}

	pub fn get_severity(&self) -> Severity {
		self.severity
	}

	pub fn get_message(&self) -> &String {
		&self.message
	}

	pub fn get_span(&self) -> Option<Span> {
		self.span
	}
}



#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
	Note,
	Warning,
	Error,
	Fatal,
}

impl Severity {
	pub fn to_str(self) -> &'static str {
		match self {
			Severity::Fatal => "fatal",
			Severity::Error => "error",
			Severity::Warning => "warning",
			Severity::Note => "note",
		}
	}
}

impl fmt::Display for Severity {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}", self.to_str())
	}
}

impl fmt::Display for DiagBuilder2 {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let colorcode = match self.get_severity() {
			Severity::Fatal | Severity::Error => "\x1B[31;1m",
			Severity::Warning => "\x1B[33;1m",
			Severity::Note => "\x1B[34;1m",
		};
		write!(f, "{}{}:\x1B[m\x1B[1m {}\x1B[m\n", colorcode, self.get_severity(), self.get_message())?;

		// Dump the part of the source file that is affected.
		if let Some(sp) = self.get_span() {
			let c = sp.source.get_content();
			let mut iter = c.extract_iter(0, sp.begin);

			// Look for the start of the line.
			let mut col = 1;
			let mut line = 1;
			let mut line_offset = sp.begin;
			while let Some(c) = iter.next_back() {
				match c.1 {
					'\n' => { line += 1; break; },
					'\r' => continue,
					_ => {
						col += 1;
						line_offset = c.0;
					}
				}
			}

			// Count the number of lines.
			while let Some(c) = iter.next_back() {
				if c.1 == '\n' {
					line += 1;
				}
			}

			// Print the line in question.
			let text: String = c.iter_from(line_offset).map(|x| x.1).take_while(|c| *c != '\n' && *c != '\r').collect();
			write!(f, "{}:{}:{}-{}:\n", sp.source.get_path(), line, col, col + sp.extract().len())?;
			for (mut i,c) in text.char_indices() {
				i += line_offset;
				if sp.begin != sp.end {
					if i == sp.begin { write!(f, "{}", colorcode)?; }
					if i == sp.end { write!(f, "\x1B[m")?; }
				}
				match c {
					'\t' => write!(f, "    ")?,
					c => write!(f, "{}", c)?,
				}
			}
			write!(f, "\n")?;

			// Print the caret markers for the line in question.
			let mut pd = ' ';
			for (mut i,c) in text.char_indices() {
				i += line_offset;
				let d = if (i >= sp.begin && i < sp.end) || (i == sp.begin && sp.begin == sp.end) {
					'^'
				} else {
					' '
				};
				if d != pd {
					write!(f, "{}", if d == ' ' {"\x1B[m"} else {colorcode})?;
				}
				pd = d;
				match c {
					'\t' => write!(f, "{}{}{}{}", d, d, d, d)?,
					_ => write!(f, "{}", d)?,
				}
			}
			write!(f, "\x1B[m\n")?;
		}
		Ok(())
	}
}
