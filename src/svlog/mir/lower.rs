// Copyright (c) 2016-2019 Fabian Schuiki

//! Lowering to MIR.

use crate::{
    crate_prelude::*,
    hir::HirNode,
    hir::PatternMapping,
    mir::rvalue::*,
    ty::{Type, TypeKind},
    value::ValueKind,
    ParamEnv,
};
use num::{BigInt, One, Signed, ToPrimitive};
use std::{cmp::max, collections::HashMap};

// TODO(fschuiki): Maybe move most of the functions below into the rvalue mod?

struct Builder<'a, C> {
    cx: &'a C,
    span: Span,
    expr: NodeId,
    env: ParamEnv,
}

impl<'a, C: Context<'a>> Builder<'_, C> {
    /// Intern an MIR node.
    fn build(&self, ty: Type<'a>, kind: RvalueKind<'a>) -> &'a Rvalue<'a> {
        self.cx.arena().alloc_mir_rvalue(Rvalue {
            id: self.cx.alloc_id(self.span),
            origin: self.expr,
            env: self.env,
            span: self.span,
            ty,
            kind: kind,
        })
    }

    /// Create an error node.
    ///
    /// This is usually called when something goes wrong during MIR construction
    /// and a marker node is needed to indicate that part of the MIR is invalid.
    fn error(&self) -> &'a Rvalue<'a> {
        self.build(self.cx.mkty_void(), RvalueKind::Error)
    }
}

/// Lower an expression to an rvalue in the MIR.
pub fn lower_expr_to_mir_rvalue<'gcx>(
    cx: &impl Context<'gcx>,
    expr_id: NodeId,
    env: ParamEnv,
) -> &'gcx Rvalue<'gcx> {
    let span = cx.span(expr_id);
    let builder = Builder {
        cx,
        span,
        expr: expr_id,
        env,
    };
    let result = || -> Result<&Rvalue> {
        let hir = match builder.cx.hir_of(expr_id)? {
            HirNode::Expr(x) => x,
            HirNode::VarDecl(decl) => unimplemented!("mir rvalue for {:?}", decl),
            HirNode::Port(port) => unimplemented!("mir rvalue for {:?}", port),
            x => unreachable!("rvalue for {:#?}", x),
        };
        let ty = builder.cx.type_of(expr_id, env)?;
        match hir.kind {
            hir::ExprKind::IntConst(..)
            | hir::ExprKind::UnsizedConst(..)
            | hir::ExprKind::TimeConst(_) => {
                let k = builder.cx.constant_value_of(expr_id, env)?;
                Ok(builder.build(k.ty, RvalueKind::Const(k)))
            }
            hir::ExprKind::Ident(_name) => {
                let binding = builder.cx.resolve_node(expr_id, env)?;
                match builder.cx.hir_of(binding)? {
                    HirNode::VarDecl(decl) => Ok(builder.build(ty, RvalueKind::Var(decl.id))),
                    HirNode::Port(port) => Ok(builder.build(ty, RvalueKind::Port(port.id))),
                    x => {
                        builder.cx.emit(
                            DiagBuilder2::error(format!(
                                "{} cannot be used in expression",
                                x.desc_full()
                            ))
                            .span(span),
                        );
                        Err(())
                    }
                }
            }
            hir::ExprKind::Binary(op, lhs, rhs) => Ok(lower_binary(&builder, ty, op, lhs, rhs)),
            hir::ExprKind::NamedPattern(ref mapping) => {
                if ty.is_array() || ty.is_bit_vector() {
                    Ok(lower_array_pattern(&builder, mapping, ty))
                } else if ty.is_struct() {
                    Ok(lower_struct_pattern(&builder, mapping, ty))
                } else {
                    builder.cx.emit(
                        DiagBuilder2::error(format!(
                            "`'{{...}}` cannot construct a value of type {}",
                            ty
                        ))
                        .span(span),
                    );
                    Err(())
                }
            }
            hir::ExprKind::Concat(repeat, ref exprs) => {
                // Compute the SBVT for each expression and lower it to MIR,
                // implicitly casting to the SBVT.
                let exprs = exprs
                    .iter()
                    .map(|&expr| {
                        let ty = builder.cx.type_of(expr, env)?;
                        let flat_ty = match map_to_simple_bit_type(builder.cx, ty, env) {
                            Some(ty) => ty,
                            None => {
                                builder.cx.emit(
                                    DiagBuilder2::error(format!(
                                        "`{}` cannot be used in concatenation",
                                        ty
                                    ))
                                    .span(builder.cx.span(expr)),
                                );
                                return Err(());
                            }
                        };
                        Ok((
                            ty::bit_size_of_type(builder.cx, ty, env)?,
                            lower_expr_and_cast(builder.cx, expr, env, flat_ty),
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;

                // Compute the result type of the concatenation.
                let total_width = exprs.iter().map(|(w, _)| w).sum();
                let result_ty = builder.cx.intern_type(TypeKind::BitVector {
                    domain: ty::Domain::FourValued, // TODO(fschuiki): check if this is correct
                    sign: ty::Sign::Unsigned,       // fixed by standard
                    range: ty::Range {
                        size: total_width,
                        dir: ty::RangeDir::Down,
                        offset: 0isize,
                    },
                    dubbed: false,
                });

                // Assemble the concatenation.
                let concat = builder.build(
                    result_ty,
                    RvalueKind::Concat(exprs.into_iter().map(|(_, v)| v).collect()),
                );

                // If a repetition is present, apply that.
                let repeat = if let Some(repeat) = repeat {
                    let count = builder
                        .cx
                        .constant_int_value_of(repeat, env)?
                        .to_usize()
                        .unwrap();
                    let total_width = total_width * count;
                    let result_ty = builder.cx.intern_type(TypeKind::BitVector {
                        domain: ty::Domain::FourValued,
                        sign: ty::Sign::Unsigned,
                        range: ty::Range {
                            size: total_width,
                            dir: ty::RangeDir::Down,
                            offset: 0isize,
                        },
                        dubbed: false,
                    });
                    builder.build(result_ty, RvalueKind::Repeat(count, concat))
                } else {
                    concat
                };
                Ok(repeat)
            }

            hir::ExprKind::Builtin(hir::BuiltinCall::Signed(id)) => {
                Ok(lower_expr_and_cast_sign(&builder, id, ty::Sign::Signed))
            }
            hir::ExprKind::Builtin(hir::BuiltinCall::Unsigned(id)) => {
                Ok(lower_expr_and_cast_sign(&builder, id, ty::Sign::Unsigned))
            }

            hir::ExprKind::Index(target, mode) => {
                // Determine the index of the LSB and the width of the
                // selection. Note that bit-selects are mapped to part-selects
                // of length 1.
                let (base, length): (&Rvalue, usize) = match mode {
                    hir::IndexMode::One(index) => (lower_expr_to_mir_rvalue(cx, index, env), 1),
                    hir::IndexMode::Many(ast::RangeMode::RelativeUp, base, delta) => (
                        lower_expr_to_mir_rvalue(cx, base, env),
                        cx.constant_int_value_of(delta, env)?.to_usize().unwrap(),
                    ),
                    hir::IndexMode::Many(ast::RangeMode::RelativeDown, base, delta) => {
                        let base = lower_expr_to_mir_rvalue(cx, base, env);
                        let delta_rvalue = lower_expr_and_cast(cx, delta, env, base.ty);
                        let base = builder.build(
                            base.ty,
                            RvalueKind::IntBinaryArith {
                                op: IntBinaryArithOp::Sub,
                                width: 0,
                                lhs: base,
                                rhs: delta_rvalue,
                            },
                        );
                        let length = cx.constant_int_value_of(delta, env)?.to_usize().unwrap();
                        (base, length)
                    }
                    hir::IndexMode::Many(ast::RangeMode::Absolute, lhs, rhs) => {
                        let lhs_int = cx.constant_int_value_of(lhs, env)?;
                        let rhs_int = cx.constant_int_value_of(rhs, env)?;
                        let base = std::cmp::min(lhs_int, rhs_int).clone();
                        let base_ty = cx.mkty_int(max(base.bits(), 1));
                        let base = cx.intern_value(value::make_int(base_ty, base));
                        let base = builder.build(base_ty, RvalueKind::Const(base));
                        let length = ((lhs_int - rhs_int).abs() + BigInt::one())
                            .to_usize()
                            .unwrap();
                        (base, length)
                    }
                };

                // Cast the indexed array to a simple bit vector type.
                let target_ty = cx.type_of(target, env)?;
                let sbvt = match map_to_simple_bit_vector_type(cx, target_ty, env) {
                    Some(ty) => ty,
                    None => {
                        let span = builder.cx.span(target);
                        builder.cx.emit(
                            DiagBuilder2::error(format!(
                                "`{}` cannot be index into",
                                span.extract()
                            ))
                            .span(span)
                            .add_note(format!(
                                "`{}` cannot has no simple bit-vector type representation",
                                target_ty
                            )),
                        );
                        return Ok(builder.error());
                    }
                };
                let target = lower_expr_and_cast(cx, target, env, sbvt);

                // Build the cast rvalue.
                Ok(builder.build(
                    ty,
                    RvalueKind::Index {
                        value: target,
                        base,
                        length,
                    },
                ))
            }

            _ => {
                error!("{:#?}", hir);
                cx.unimp_msg("lowering to mir rvalue of", hir)
            }
        }
    }();
    result.unwrap_or_else(|_| builder.error())
}

/// Lower an HIR expression and implicitly cast to a target type.
fn lower_expr_and_cast<'gcx>(
    cx: &impl Context<'gcx>,
    expr_id: NodeId,
    env: ParamEnv,
    target_ty: Type<'gcx>,
) -> &'gcx Rvalue<'gcx> {
    let inner = lower_expr_to_mir_rvalue(cx, expr_id, env);
    let builder = Builder {
        cx,
        span: inner.span,
        expr: expr_id,
        env,
    };
    lower_implicit_cast(&builder, inner, target_ty)
}

/// Lower an HIR expression and implicitly cast to a different sign.
fn lower_expr_and_cast_sign<'gcx>(
    builder: &Builder<'_, impl Context<'gcx>>,
    expr_id: NodeId,
    sign: ty::Sign,
) -> &'gcx Rvalue<'gcx> {
    let inner = lower_expr_to_mir_rvalue(builder.cx, expr_id, builder.env);
    if let Some(ty) = map_to_simple_bit_type(builder.cx, inner.ty.resolve_name(), builder.env) {
        let ty = change_type_sign(builder.cx, ty, sign);
        lower_implicit_cast(builder, inner, ty)
    } else {
        let span = builder.cx.span(expr_id);
        builder.cx.emit(
            DiagBuilder2::error(format!("`{}` cannot be sign-cast", span.extract())).span(span),
        );
        builder.error()
    }
}

/// Generate the nodes necessary to implicitly cast and rvalue to a type.
///
/// If the cast is not possible, emit some helpful diagnostics.
fn lower_implicit_cast<'gcx>(
    builder: &Builder<'_, impl Context<'gcx>>,
    value: &'gcx Rvalue<'gcx>,
    to: Type<'gcx>,
) -> &'gcx Rvalue<'gcx> {
    let from = value.ty;

    // Catch the easy case where the types already line up.
    if from == to {
        return value;
    }

    // Strip away all named types.
    let from_raw = from.resolve_name();
    let to_raw = to.resolve_name();
    trace!(
        "trying implicit cast {:?} from {:?} to {:?}",
        value,
        from_raw,
        to_raw
    );

    // Try a value domain cast.
    let from_domain = from_raw.get_value_domain();
    let to_domain = to_raw.get_value_domain();
    if from_domain.is_some() && to_domain.is_some() && from_domain != to_domain {
        let fd = from_domain.unwrap();
        let td = to_domain.unwrap();
        let target_ty = match *from_raw {
            TypeKind::Bit(_) => builder.cx.intern_type(TypeKind::Bit(td)),
            TypeKind::Int(w, _) => builder.cx.intern_type(TypeKind::Int(w, td)),
            TypeKind::BitScalar { sign, .. } => builder
                .cx
                .intern_type(TypeKind::BitScalar { domain: td, sign }),
            TypeKind::BitVector {
                sign,
                range,
                dubbed,
                ..
            } => builder.cx.intern_type(TypeKind::BitVector {
                domain: td,
                sign,
                range,
                dubbed,
            }),
            _ => unreachable!(),
        };
        let inner = builder.build(
            target_ty,
            RvalueKind::CastValueDomain {
                from: fd,
                to: td,
                value,
            },
        );
        return lower_implicit_cast(builder, inner, to);
    }

    // Try a truncation or extension cast.
    // let get_width_and_sign = |ty: Type| match *ty {
    //     TypeKind::Bit(_) => Some((1, ty::Sign::Unsigned)),
    //     TypeKind::Int(w, _) => Some((w, ty::Sign::Unsigned)),
    //     TypeKind::BitScalar { sign, .. } => Some((1, sign)),
    //     TypeKind::BitVector { sign, range, .. } => Some((range.size, sign)),
    //     _ => None,
    // };
    let from_sbvt = map_to_simple_bit_vector_type(builder.cx, from_raw, builder.env);
    let to_sbvt = map_to_simple_bit_vector_type(builder.cx, to_raw, builder.env);
    let from_size = from_sbvt.map(|ty| ty.width());
    let to_size = to_sbvt.map(|ty| ty.width());

    if from_size.is_some() && to_size.is_some() && from_size != to_size {
        let from_sbvt = from_sbvt.unwrap();
        let value = lower_implicit_cast(builder, value, from_sbvt);
        let from_size = from_size.unwrap();
        let to_size = to_size.unwrap();
        let range = match *to_sbvt.unwrap() {
            TypeKind::BitVector { range, .. } => range,
            _ => unreachable!(),
        };
        let sign = from_sbvt.get_sign().unwrap();
        let ty = builder.cx.intern_type(TypeKind::BitVector {
            domain: from_sbvt.get_value_domain().unwrap(),
            sign,
            range,
            dubbed: false,
        });
        let kind = if from_size < to_size {
            match sign {
                ty::Sign::Signed => RvalueKind::SignExtend(range.size, value),
                ty::Sign::Unsigned => RvalueKind::ZeroExtend(range.size, value),
            }
        } else {
            RvalueKind::Truncate(range.size, value)
        };
        let inner = builder.build(ty, kind);
        return lower_implicit_cast(builder, inner, to);
    }

    // Try a single-bit atom/vector conversion.
    let from_sbt = map_to_simple_bit_type(builder.cx, from_raw, builder.env);
    let to_sbt = map_to_simple_bit_type(builder.cx, to_raw, builder.env);
    match (from_sbt, to_sbt) {
        (
            Some(&TypeKind::BitVector {
                domain,
                sign,
                range: ty::Range { size: 1, .. },
                ..
            }),
            Some(&TypeKind::BitScalar { .. }),
        ) => {
            let value = lower_implicit_cast(builder, value, from_sbt.unwrap());
            let inner = builder.build(
                builder.cx.intern_type(TypeKind::BitScalar { domain, sign }),
                RvalueKind::CastVectorToAtom { domain, value },
            );
            return lower_implicit_cast(builder, inner, to);
        }
        (Some(&TypeKind::BitScalar { domain, sign }), Some(&TypeKind::BitVector { .. })) => {
            let value = lower_implicit_cast(builder, value, from_sbt.unwrap());
            let inner = builder.build(
                builder.cx.intern_type(TypeKind::BitVector {
                    domain,
                    sign,
                    range: ty::Range {
                        size: 1,
                        dir: ty::RangeDir::Down,
                        offset: 0isize,
                    },
                    dubbed: false,
                }),
                RvalueKind::CastAtomToVector { domain, value },
            );
            return lower_implicit_cast(builder, inner, to);
        }
        _ => (),
    }

    // Try a sign cast.
    let from_sign = from_sbt.and_then(|ty| ty.get_sign());
    let to_sign = to_sbt.and_then(|ty| ty.get_sign());
    if from_sign.is_some() && to_sign.is_some() && from_sign != to_sign {
        let value = lower_implicit_cast(builder, value, from_sbt.unwrap());
        let ty = change_type_sign(builder.cx, from_sbt.unwrap(), to_sign.unwrap());
        let inner = builder.build(ty, RvalueKind::CastSign(to_sign.unwrap(), value));
        return lower_implicit_cast(builder, inner, to);
    }

    // Try to make the cast happen.
    match (from_raw, to_raw) {
        // Integer to bit truncation; e.g. `int` to `bit`.
        (&TypeKind::Int(fw, fd), &TypeKind::Bit(_)) if fw > 1 => {
            // trace!("would narrow {} int to 1 bit", fw);
            let inner = builder.build(
                builder.cx.intern_type(TypeKind::Int(1, fd)),
                RvalueKind::Truncate(1, value),
            );
            return lower_implicit_cast(builder, inner, to);
        }

        // // Integer to bit conversion; e.g. `bit [0:0]` to `bit`.
        // (&TypeKind::Int(fw, fd), &TypeKind::Bit(_)) if fw == 1 => {
        //     // trace!("would map int to bit");
        //     let inner = builder.build(
        //         builder.cx.intern_type(TypeKind::Bit(fd)),
        //         RvalueKind::CastVectorToAtom { domain: fd, value },
        //     );
        //     return lower_implicit_cast(builder, inner, to);
        // }

        // // Bit vector truncation and zero and sign extension.
        // (
        //     &TypeKind::BitVector {
        //         domain,
        //         sign,
        //         range: ty::Range { size: fw, .. },
        //         ..
        //     },
        //     &TypeKind::BitVector { range, .. },
        // ) if fw != range.size => {
        //     let ty = builder.cx.intern_type(TypeKind::BitVector {
        //         domain,
        //         sign,
        //         range,
        //         dubbed: false,
        //     });
        //     let kind = if fw < range.size {
        //         match sign {
        //             ty::Sign::Signed => RvalueKind::SignExtend(range.size, value),
        //             ty::Sign::Unsigned => RvalueKind::ZeroExtend(range.size, value),
        //         }
        //     } else {
        //         RvalueKind::Truncate(range.size, value)
        //     };
        //     let inner = builder.build(ty, kind);
        //     return lower_implicit_cast(builder, inner, to);
        // }

        // TODO(fschuiki): Packing structs into bit vectors.
        // TODO(fschuiki): Unpacking structs from bit vectors.
        // TODO(fschuiki): Integer truncation.
        // TODO(fschuiki): Array truncation.
        // TODO(fschuiki): Array extension.
        // TODO(fschuiki): Signed/unsigned conversion.
        _ => (),
    }

    // Complain and abort.
    error!("failed implicit cast from {:?} to {:?}", from, to);
    info!("failed implicit cast from {:?}", value);
    builder.cx.emit(
        DiagBuilder2::error(format!(
            "type `{}` required, but expression has type `{}`",
            to, from
        ))
        .span(value.span),
    );
    builder.error()
}

/// Change the sign of a simple bit type.
fn change_type_sign<'gcx>(cx: &impl Context<'gcx>, ty: Type<'gcx>, sign: ty::Sign) -> Type<'gcx> {
    match *ty {
        TypeKind::BitScalar { domain, .. } => cx.intern_type(TypeKind::BitScalar { domain, sign }),
        TypeKind::BitVector {
            domain,
            range,
            dubbed,
            ..
        } => cx.intern_type(TypeKind::BitVector {
            domain,
            sign,
            range,
            dubbed,
        }),
        _ => ty,
    }
}

/// Lower a `'{...}` array pattern.
fn lower_array_pattern<'gcx>(
    builder: &Builder<'_, impl Context<'gcx>>,
    mapping: &[(PatternMapping, NodeId)],
    ty: Type<'gcx>,
) -> &'gcx Rvalue<'gcx> {
    let (length, offset, elem_ty) = match *ty {
        TypeKind::PackedArray(w, t) => (w, 0isize, t),
        TypeKind::BitScalar { domain, .. } => (1, 0isize, domain.bit_type()),
        TypeKind::BitVector { domain, range, .. } => (range.size, range.offset, domain.bit_type()),
        _ => unreachable!("array pattern with invalid input type"),
    };
    let mut failed = false;
    let mut default: Option<&Rvalue> = None;
    let mut values = HashMap::<usize, &Rvalue>::new();
    for &(map, to) in mapping {
        match map {
            PatternMapping::Type(type_id) => {
                builder.cx.emit(
                    DiagBuilder2::error("types cannot index into an array")
                        .span(builder.cx.span(type_id)),
                );
                failed = true;
                continue;
            }
            PatternMapping::Member(member_id) => {
                // Determine the index for the mapping.
                let index = match || -> Result<usize> {
                    let index = builder.cx.constant_value_of(member_id, builder.env)?;
                    let index = match &index.kind {
                        ValueKind::Int(i) => i - num::BigInt::from(offset),
                        _ => {
                            builder.cx.emit(
                                DiagBuilder2::error("array index must be a constant integer")
                                    .span(builder.cx.span(member_id)),
                            );
                            return Err(());
                        }
                    };
                    let index = match index.to_isize() {
                        Some(i) if i >= 0 && i < length as isize => i as usize,
                        _ => {
                            builder.cx.emit(
                                DiagBuilder2::error(format!("index `{}` out of bounds", index))
                                    .span(builder.cx.span(member_id)),
                            );
                            return Err(());
                        }
                    };
                    Ok(index)
                }() {
                    Ok(i) => i,
                    Err(_) => {
                        failed = true;
                        continue;
                    }
                };

                // Determine the value and insert into the mappings.
                let value = lower_expr_and_cast(builder.cx, to, builder.env, elem_ty);
                let span = value.span;
                if let Some(prev) = values.insert(index, value) {
                    builder.cx.emit(
                        DiagBuilder2::warning(format!(
                            "`{}` overwrites previous value `{}` at index {}",
                            span.extract(),
                            prev.span.extract(),
                            index
                        ))
                        .span(span)
                        .add_note("Previous value was here:")
                        .span(prev.span),
                    );
                }
            }
            PatternMapping::Default => match default {
                Some(ref default) => {
                    builder.cx.emit(
                        DiagBuilder2::error("pattern has multiple default mappings")
                            .span(builder.cx.span(to))
                            .add_note("Previous mapping default mapping was here:")
                            .span(default.span),
                    );
                    failed = true;
                    continue;
                }
                None => {
                    default = Some(lower_expr_and_cast(builder.cx, to, builder.env, elem_ty));
                }
            },
        }
    }

    // In case the list of indices provided by the user is incomplete, use the
    // default to fill in the other elements.
    if values.len() != length {
        let default = if let Some(default) = default {
            default
        } else {
            builder.cx.emit(
                DiagBuilder2::error("`default:` missing in non-exhaustive array pattern")
                    .span(builder.span)
                    .add_note("Array patterns must assign a value to every index."),
            );
            return builder.error();
        };
        for i in 0..length {
            if values.contains_key(&i) {
                continue;
            }
            values.insert(i, default);
        }
    }

    match *ty {
        _ if failed => builder.error(),
        TypeKind::PackedArray(..) => builder.build(ty, RvalueKind::ConstructArray(values)),
        TypeKind::BitScalar { .. } => {
            assert_eq!(values.len(), 1);
            values[&0]
        }
        TypeKind::BitVector { .. } => builder.build(
            ty,
            RvalueKind::Concat((0..length).rev().map(|i| values[&i]).collect()),
        ),
        _ => unreachable!("array pattern with invalid input type"),
    }
}

/// Lower a `'{...}` struct pattern.
fn lower_struct_pattern<'gcx>(
    builder: &Builder<'_, impl Context<'gcx>>,
    mapping: &[(PatternMapping, NodeId)],
    ty: Type<'gcx>,
) -> &'gcx Rvalue<'gcx> {
    // Determine the field names and types for the struct to be assembled.
    let def_id = ty.get_struct_def().unwrap();
    let def = match builder.cx.struct_def(def_id) {
        Ok(d) => d,
        Err(()) => return builder.error(),
    };
    let fields: Vec<_> = match def
        .fields
        .iter()
        .map(|f| Ok((f.name, builder.cx.map_to_type(f.ty, builder.env)?)))
        .collect::<Result<Vec<_>>>()
    {
        Ok(d) => d,
        Err(()) => return builder.error(),
    };
    let name_lookup: HashMap<Name, usize> = fields
        .iter()
        .enumerate()
        .map(|(i, f)| (f.0.value, i))
        .collect();
    trace!("struct fields are {:?}", fields);
    trace!("struct field names are {:?}", name_lookup);

    // Disassemble the user's mapping into actual field bindings and defaults.
    let mut failed = false;
    let mut default: Option<NodeId> = None;
    let mut type_defaults = HashMap::<Type, &Rvalue>::new();
    let mut values = HashMap::<usize, &Rvalue>::new();
    for &(map, to) in mapping {
        match map {
            PatternMapping::Type(type_id) => match builder.cx.map_to_type(type_id, builder.env) {
                Ok(ty) => {
                    let value = lower_expr_and_cast(builder.cx, to, builder.env, ty);
                    type_defaults.insert(ty, value);
                }
                Err(()) => {
                    failed = true;
                    continue;
                }
            },
            PatternMapping::Member(member_id) => match builder.cx.hir_of(member_id) {
                Ok(HirNode::Expr(&hir::Expr {
                    kind: hir::ExprKind::Ident(name),
                    ..
                })) => {
                    // Determine the index for the mapping.
                    let index = match name_lookup.get(&name.value) {
                        Some(&index) => index,
                        None => {
                            builder.cx.emit(
                                DiagBuilder2::error(format!("`{}` member does not exist", name))
                                    .span(name.span)
                                    .add_note("Struct definition was here:")
                                    .span(builder.cx.span(def_id)),
                            );
                            failed = true;
                            continue;
                        }
                    };

                    // Determine the value and insert into the mappings.
                    let value = lower_expr_and_cast(builder.cx, to, builder.env, fields[index].1);
                    let span = value.span;
                    if let Some(prev) = values.insert(index, value) {
                        builder.cx.emit(
                            DiagBuilder2::warning(format!(
                                "`{}` overwrites previous value `{}` for member `{}`",
                                span.extract(),
                                prev.span.extract(),
                                name
                            ))
                            .span(span)
                            .add_note("Previous value was here:")
                            .span(prev.span),
                        );
                    }
                }
                Ok(_) => {
                    let span = builder.cx.span(member_id);
                    builder.cx.emit(
                        DiagBuilder2::error(format!(
                            "`{}` is not a valid struct member name",
                            span.extract()
                        ))
                        .span(span),
                    );
                    failed = true;
                    continue;
                }
                Err(()) => {
                    failed = true;
                    continue;
                }
            },
            PatternMapping::Default => match default {
                Some(default) => {
                    builder.cx.emit(
                        DiagBuilder2::error("pattern has multiple default mappings")
                            .span(builder.cx.span(to))
                            .add_note("Previous mapping default mapping was here:")
                            .span(builder.cx.span(default)),
                    );
                    failed = true;
                    continue;
                }
                None => {
                    default = Some(to);
                }
            },
        }
    }

    // In case the list of members provided by the user is incomplete, use the
    // defaults to fill in the other members.
    for (index, &(field_name, field_ty)) in fields.iter().enumerate() {
        if values.contains_key(&index) {
            continue;
        }

        // Try the type-based defaults first.
        // TODO(fschuiki): Use better type comparison mechanism that is
        // transparent to user defined types, etc.
        if let Some(default) = type_defaults.get(field_ty) {
            trace!(
                "applying type default to member `{}`: {:?}",
                field_name,
                default
            );
            values.insert(index, default);
            continue;
        }

        // Try to assign a default value.
        let default = if let Some(default) = default {
            default
        } else {
            builder.cx.emit(
                DiagBuilder2::error(format!("`{}` member missing in struct pattern", field_name))
                    .span(builder.span)
                    .add_note("Struct patterns must assign a value to every member."),
            );
            failed = true;
            continue;
        };
        let value = lower_expr_and_cast(builder.cx, default, builder.env, field_ty);
        values.insert(index, value);
    }

    if failed {
        builder.error()
    } else {
        builder.build(
            ty,
            RvalueKind::ConstructStruct((0..values.len()).map(|i| values[&i]).collect()),
        )
    }
}

/// Try to convert a type to its equivalent simple bit vector type.
///
/// All *integral* data types have an equivalent *simple bit vector type*. These
/// include the following:
///
/// - all basic integers
/// - packed arrays
/// - packed structures
/// - packed unions
/// - enums
/// - time (excluded in this implementation)
fn map_to_simple_bit_type<'gcx>(
    cx: &impl Context<'gcx>,
    ty: Type<'gcx>,
    env: ParamEnv,
) -> Option<Type<'gcx>> {
    let bits = match *ty {
        TypeKind::Void => return None,
        TypeKind::Time => return None,
        TypeKind::Named(_, _, ty) => return map_to_simple_bit_type(cx, ty, env),
        TypeKind::BitVector { .. } => return Some(ty),
        TypeKind::BitScalar { .. } => return Some(ty),
        TypeKind::Bit(..)
        | TypeKind::Int(..)
        | TypeKind::Struct(..)
        | TypeKind::PackedArray(..) => ty::bit_size_of_type(cx, ty, env).ok()?,
    };
    Some(cx.intern_type(TypeKind::BitVector {
        domain: ty::Domain::FourValued, // TODO(fschuiki): check if this is correct
        sign: ty::Sign::Unsigned,
        range: ty::Range {
            size: bits,
            dir: ty::RangeDir::Down,
            offset: 0isize,
        },
        dubbed: false,
    }))
}

/// Same as `map_to_simple_bit_type`, but forces the result to be a vector.
///
/// This operation would commonly be used to cast the operand of an operator
/// which expects a vector. E.g. `foo[4]`.
fn map_to_simple_bit_vector_type<'gcx>(
    cx: &impl Context<'gcx>,
    ty: Type<'gcx>,
    env: ParamEnv,
) -> Option<Type<'gcx>> {
    match map_to_simple_bit_type(cx, ty, env) {
        Some(&TypeKind::BitScalar { domain, sign }) => Some(cx.intern_type(TypeKind::BitVector {
            domain,
            sign,
            range: ty::Range {
                size: 1,
                dir: ty::RangeDir::Down,
                offset: 0isize,
            },
            dubbed: false,
        })),
        x => x,
    }
}

/// Map a binary operator to MIR.
fn lower_binary<'gcx>(
    builder: &Builder<'_, impl Context<'gcx>>,
    ty: Type<'gcx>,
    op: hir::BinaryOp,
    lhs: NodeId,
    rhs: NodeId,
) -> &'gcx Rvalue<'gcx> {
    // Determine the simple bit vector type for the operator.
    let result_ty = match map_to_simple_bit_type(builder.cx, ty, builder.env) {
        Some(ty) => ty,
        None => {
            builder.cx.emit(
                DiagBuilder2::error(format!("`{:?}` cannot operate on `{}`", op, ty))
                    .span(builder.span),
            );
            return builder.error();
        }
    };

    // Cast the operands to the operator type.
    trace!("binary {:?} on {} maps to {}", op, ty, result_ty);
    let lhs = lower_expr_and_cast(builder.cx, lhs, builder.env, result_ty);
    let rhs = lower_expr_and_cast(builder.cx, rhs, builder.env, result_ty);

    // Determine the operation.
    let op = match op {
        hir::BinaryOp::Add => IntBinaryArithOp::Add,
        hir::BinaryOp::Sub => IntBinaryArithOp::Sub,
        hir::BinaryOp::Mul => IntBinaryArithOp::Mul,
        hir::BinaryOp::Div => IntBinaryArithOp::Div,
        hir::BinaryOp::Mod => IntBinaryArithOp::Mod,
        hir::BinaryOp::Pow => IntBinaryArithOp::Pow,
        _ => unimplemented!("mir for integral operator {:?}", op),
    };

    // Assemble the node.
    builder.build(
        result_ty,
        RvalueKind::IntBinaryArith {
            op,
            width: ty::bit_size_of_type(builder.cx, result_ty, builder.env).unwrap(),
            lhs,
            rhs,
        },
    )
}