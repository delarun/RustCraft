//! Drops a compiled function's unused locals.
//!
//! The generator lays out locals by formula (four per operand-stack depth,
//! one for each type; pools for inlined frames), and only a fraction is
//! used. Engines pay for every declared local: a baseline tier (V8's
//! Liftoff) zeroes them all on each call and gives each a stack slot. So the
//! body is rewritten with the used locals renumbered densely.

use wasmparser::{BinaryReader, FunctionBody, Operator};

/// `function`'s body (as from `Function::into_raw_body`) with only the
/// parameters and the locals it uses; `params` is its parameter count.
/// Returns the new body and its number of declared (non-parameter) locals.
pub fn compact(body: &[u8], params: u32) -> Result<(Vec<u8>, u32), String> {
    let error = |e: wasmparser::BinaryReaderError| e.to_string();
    let function = FunctionBody::new(BinaryReader::new(body, 0));
    let mut types = Vec::new();
    for local in function.get_locals_reader().map_err(error)? {
        let (count, ty) = local.map_err(error)?;
        types.extend(std::iter::repeat_n(ty, count as usize));
    }
    // Operators with their byte ranges in `body`.
    let mut reader = function.get_operators_reader().map_err(error)?;
    let mut ops = Vec::new();
    let mut used = vec![false; types.len()];
    while !reader.eof() {
        let (op, at) = reader.read_with_offset().map_err(error)?;
        if let Operator::LocalGet { local_index } | Operator::LocalSet { local_index } | Operator::LocalTee { local_index } = op {
            if local_index >= params {
                used[(local_index - params) as usize] = true;
            }
            ops.push((at as usize, Some((op_code(&op), local_index))));
        } else {
            ops.push((at as usize, None));
        }
    }
    let end = body.len();
    // Old index (beyond the parameters) → new.
    let mut renumbered = vec![u32::MAX; types.len()];
    let mut declared: Vec<(u32, wasm_encoder::ValType)> = Vec::new();
    let mut next = params;
    for (i, ty) in types.iter().enumerate() {
        if !used[i] {
            continue;
        }
        renumbered[i] = next;
        next += 1;
        let ty = encoder_type(*ty)?;
        match declared.last_mut() {
            Some((count, last)) if *last == ty => *count += 1,
            _ => declared.push((1, ty)),
        }
    }
    let mut out = Vec::with_capacity(body.len());
    leb(&mut out, declared.len() as u32);
    for &(count, ty) in &declared {
        leb(&mut out, count);
        wasm_encoder::Encode::encode(&ty, &mut out);
    }
    for (k, &(at, local)) in ops.iter().enumerate() {
        let until = ops.get(k + 1).map_or(end, |&(next, _)| next);
        match local {
            Some((code, index)) => {
                out.push(code);
                leb(&mut out, if index < params { index } else { renumbered[(index - params) as usize] });
            }
            None => out.extend_from_slice(&body[at..until]),
        }
    }
    Ok((out, next - params))
}

fn op_code(op: &Operator) -> u8 {
    match op {
        Operator::LocalGet { .. } => 0x20,
        Operator::LocalSet { .. } => 0x21,
        _ => 0x22,
    }
}

fn encoder_type(ty: wasmparser::ValType) -> Result<wasm_encoder::ValType, String> {
    Ok(match ty {
        wasmparser::ValType::I32 => wasm_encoder::ValType::I32,
        wasmparser::ValType::I64 => wasm_encoder::ValType::I64,
        wasmparser::ValType::F32 => wasm_encoder::ValType::F32,
        wasmparser::ValType::F64 => wasm_encoder::ValType::F64,
        other => return Err(format!("unexpected local type {other:?}")),
    })
}

fn leb(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
