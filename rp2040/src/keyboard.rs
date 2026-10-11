//! Bounded HID keyboard decoding. No device identity whitelist or text conversion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyReport { pub modifiers: u8, pub keys: [u8; 6] }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Physical { pub bits: [u32; 8], pub modifiers: u8 }
impl Physical {
    pub fn contains(&self, key: u8) -> bool { self.bits[key as usize / 32] & (1 << (key % 32)) != 0 }
    pub fn insert(&mut self, key: u8) { self.bits[key as usize / 32] |= 1 << (key % 32); }
    pub fn empty(&self) -> bool { self.modifiers == 0 && self.bits.iter().all(|&v| v == 0) }
    pub fn retain_held(&mut self, held: Self) {
        self.modifiers &= held.modifiers;
        for (b, h) in self.bits.iter_mut().zip(held.bits) { *b &= h; }
    }
    pub fn without(self, blocked: Self) -> Self {
        let mut out = self;
        out.modifiers &= !blocked.modifiers;
        for (b, mask) in out.bits.iter_mut().zip(blocked.bits) { *b &= !mask; }
        out.bits[2] &= !(1 << 7); // Scroll Lock (0x47) is never forwarded.
        out
    }
    pub fn select(self, previous: KeyReport) -> KeyReport {
        let mut out = KeyReport { modifiers: self.modifiers, keys: [0; 6] };
        let mut count = 0;
        for key in previous.keys {
            if key > 3 && key != 0x47 && self.contains(key) && !out.keys.contains(&key) {
                out.keys[count] = key; count += 1;
            }
        }
        for key in 4..=255u8 {
            if count == 6 { break; }
            if key != 0x47 && self.contains(key) && !out.keys.contains(&key) {
                out.keys[count] = key; count += 1;
            }
        }
        out.keys[..count].sort_unstable(); out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decode { State(Physical), Rollover, Other, Invalid }

pub fn boot(data: &[u8]) -> Decode {
    if data.len() != 8 { return Decode::Invalid; }
    let mut p = Physical { modifiers: data[0], ..Physical::default() };
    for &key in &data[2..] {
        if (1..=3).contains(&key) { return Decode::Rollover; }
        set_usage(&mut p, key);
    }
    Decode::State(p)
}

#[derive(Clone, Copy, Default)]
struct Field {
    id: u8, offset: u16, size: u8, count: u16, variable: bool,
    minimum: u16, maximum: u16, usage_offset: u16, usage_count: u16,
    logical_min: i32, logical_max: i32, null_state: bool,
}
pub struct Descriptor {
    fields: [Field; 24], count: usize, numbered: bool,
    input_bits: [u16; 256],
    usages: [u16; 512], usage_len: usize,
}
#[derive(Clone, Copy, Default)]
struct Global { page: u16, size: u32, count: u32, id: u8, logical_min: i32, logical_max: i32 }
struct Local { usages: [u16; 256], count: usize, min: u16, max: u16 }
impl Default for Local {
    fn default() -> Self { Self { usages: [0; 256], count: 0, min: 0, max: 0 } }
}
impl Local {
    fn reset(&mut self) { self.count = 0; self.min = 0; self.max = 0; }
}

impl Descriptor {
    fn usage(&self, field: &Field, index: usize) -> Option<u16> {
        if field.usage_count != 0 {
            let index = if field.variable { index.min(field.usage_count as usize - 1) } else { index };
            (index < field.usage_count as usize).then(|| self.usages[field.usage_offset as usize + index])
        } else {
            if field.minimum > field.maximum { return None; }
            let index = if field.variable { index.min((field.maximum - field.minimum) as usize) } else { index };
            let u = field.minimum.checked_add(u16::try_from(index).ok()?)?;
            (u <= field.maximum).then_some(u)
        }
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.is_empty() || bytes.len() > 1024 { return Err("descriptor length"); }
        let mut out = Self { fields: [Field::default(); 24], count: 0, numbered: false,
            input_bits: [0; 256],
            usages: [0; 512], usage_len: 0 };
        let mut global = Global::default(); let mut local = Local::default();
        let mut stack = [Global::default(); 4]; let mut sp = 0;
        let mut depth = 0usize; let mut keyboard_depth = 0usize; let mut found = false;
        let mut pos = 0;
        while pos < bytes.len() {
            let prefix = bytes[pos]; pos += 1;
            if prefix == 0xfe { return Err("long item"); }
            let size = match prefix & 3 { 3 => 4, n => n as usize };
            if pos + size > bytes.len() { return Err("truncated item"); }
            let mut value = 0u32;
            for i in 0..size { value |= (bytes[pos + i] as u32) << (i * 8); } pos += size;
            let tag = prefix >> 4;
            match (prefix >> 2 & 3, tag) {
                (1, 0) => global.page = u16::try_from(value).map_err(|_| "usage page")?,
                (1, 1) => global.logical_min = signed(value, size),
                (1, 2) => global.logical_max = if global.logical_min < 0 { signed(value, size) } else { i32::try_from(value).map_err(|_| "logical maximum")? },
                (1, 7) => global.size = value,
                (1, 8) => { if value == 0 || value > 255 { return Err("report ID"); } global.id = value as u8; out.numbered = true; }
                (1, 9) => global.count = value,
                (1, 10) => { if sp == stack.len() { return Err("global stack"); } stack[sp] = global; sp += 1; }
                (1, 11) => { if sp == 0 { return Err("global stack"); } sp -= 1; global = stack[sp]; }
                (2, 0) => {
                    if size == 4 && value >> 16 != global.page as u32 { return Err("mixed usage page"); }
                    if local.count == local.usages.len() { return Err("usage list"); }
                    local.usages[local.count] = value as u16; local.count += 1;
                }
                (2, 1 | 2) => {
                    if size == 4 && value >> 16 != global.page as u32 { return Err("mixed usage page"); }
                    if tag == 1 { local.min = value as u16; } else { local.max = value as u16; }
                }
                (2, 10) => return Err("usage delimiter"),
                (0, 10) => {
                    depth += 1;
                    if !found && global.page == 1 && local.count > 0 && local.usages[0] == 6 && value == 1 {
                        found = true; keyboard_depth = depth;
                    }
                    local.reset();
                }
                (0, 12) => {
                    if depth == 0 { return Err("collection underflow"); }
                    if depth == keyboard_depth { keyboard_depth = 0; }
                    depth -= 1; local.reset();
                }
                (0, 8 | 9 | 11) => {
                    let total = global.size.checked_mul(global.count).ok_or("report size")?;
                    if global.size > 32 || total > 2048 { return Err("report size"); }
                    if tag != 8 { local.reset(); continue; } // Output/feature reports are never sent.
                    let id = global.id as usize;
                    let offset = out.input_bits[id];
                    out.input_bits[id] = offset.checked_add(total as u16).filter(|&v| v <= 2048).ok_or("report size")?;
                    if keyboard_depth != 0 && value & 1 == 0 && global.page == 7 {
                        if global.size == 0 || global.count == 0 || global.count > 256 { return Err("keyboard field"); }
                        if value & 4 != 0 || (local.count == 0 && local.min > local.max) { return Err("keyboard layout"); }
                        if value & 2 == 0 && (global.logical_min < 0 || global.logical_max < global.logical_min) { return Err("array logical range"); }
                        if out.usage_len + local.count > out.usages.len() { return Err("usage pool"); }
                        out.usages[out.usage_len..out.usage_len + local.count].copy_from_slice(&local.usages[..local.count]);
                        let field = Field { id: global.id, offset, size: global.size as u8, count: global.count as u16,
                            variable: value & 2 != 0, minimum: local.min, maximum: local.max,
                            usage_offset: out.usage_len as u16, usage_count: local.count as u16,
                            logical_min: global.logical_min, logical_max: global.logical_max, null_state: value & 0x40 != 0 };
                        out.usage_len += local.count;
                        if out.count == out.fields.len() { return Err("keyboard fields"); }
                        out.fields[out.count] = field; out.count += 1;
                    }
                    local.reset();
                }
                (0, _) => local.reset(),
                _ => (),
            }
        }
        if !found { return Err("no keyboard collection"); }
        if depth != 0 || sp != 0 || out.count == 0 { return Err("invalid keyboard collection"); }
        if out.numbered && out.fields[..out.count].iter().any(|f| f.id == 0) { return Err("mixed report IDs"); }
        if out.numbered && out.fields[..out.count].iter().any(|f| out.input_bits[f.id as usize] > 2040) { return Err("numbered report length"); }
        Ok(out)
    }
    pub fn decode(&self, bytes: &[u8], previous: Physical) -> Decode {
        let (id, data) = if self.numbered { match bytes.split_first() { Some((&id, data)) => (id, data), None => return Decode::Invalid } } else { (0, bytes) };
        if !self.fields[..self.count].iter().any(|f| f.id == id) { return Decode::Other; }
        if data.len() * 8 != (self.input_bits[id as usize] as usize).div_ceil(8) * 8 { return Decode::Invalid; }
        // Preserve keyboard fields belonging to other report IDs, replace only this report's fields.
        let mut out = previous;
        for f in self.fields[..self.count].iter().filter(|f| f.id == id) {
            if f.variable {
                for i in 0..f.count as usize {
                    if let Some(u) = self.usage(f, i) { clear_usage(&mut out, u); }
                }
            } else {
                // Arrays describe a complete range/list, not just currently pressed usages.
                if f.usage_count != 0 {
                    for i in 0..f.usage_count as usize { clear_usage(&mut out, self.usage(f, i).unwrap_or(0)); }
                } else {
                    for u in f.minimum..=f.maximum.min(255) { clear_usage(&mut out, u); }
                }
            }
        }
        for f in self.fields[..self.count].iter().filter(|f| f.id == id) {
            for i in 0..f.count as usize {
                let value = bits(data, f.offset as usize + i * f.size as usize, f.size as usize);
                let usage = if f.variable {
                    if value == 0 { continue; }
                    match self.usage(f, i) { Some(u) => u as u32, None => continue }
                } else {
                    let value = if f.logical_min < 0 { sign_bits(value, f.size as usize) } else { value as i32 };
                    if value < f.logical_min || value > f.logical_max {
                        if f.null_state { continue; } else { return Decode::Invalid; }
                    }
                    match self.usage(f, (value - f.logical_min) as usize) {
                        Some(u) => u as u32, None if f.null_state => continue, None => return Decode::Invalid,
                    }
                };
                if (1..=3).contains(&usage) { return Decode::Rollover; }
                if usage > 255 { return Decode::Invalid; }
                set_usage(&mut out, usage as u8);
            }
        }
        Decode::State(out)
    }
}
fn signed(value: u32, bytes: usize) -> i32 {
    sign_bits(value, bytes * 8)
}
fn sign_bits(value: u32, bits: usize) -> i32 {
    if bits == 0 { 0 } else { let shift = 32 - bits; ((value << shift) as i32) >> shift }
}
fn clear_usage(p: &mut Physical, u: u16) {
    if (0xe0..=0xe7).contains(&u) { p.modifiers &= !(1 << (u - 0xe0)); }
    else if u <= 255 { p.bits[u as usize / 32] &= !(1 << (u % 32)); }
}
fn set_usage(p: &mut Physical, u: u8) {
    if (0xe0..=0xe7).contains(&u) { p.modifiers |= 1 << (u - 0xe0); }
    else if u > 3 { p.insert(u); }
}
fn bits(data: &[u8], offset: usize, size: usize) -> u32 {
    let mut value = 0;
    for i in 0..size { value |= (((data[(offset + i) / 8] >> ((offset + i) % 8)) & 1) as u32) << i; }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    pub const STANDARD: &[u8] = &[0x05,1,0x09,6,0xa1,1,0x05,7,0x19,0xe0,0x29,0xe7,0x15,0,0x25,1,0x75,1,0x95,8,0x81,2,0x95,1,0x75,8,0x81,1,0x95,5,0x75,1,0x05,8,0x19,1,0x29,5,0x91,2,0x95,1,0x75,3,0x91,1,0x95,6,0x75,8,0x15,0,0x25,0x65,0x05,7,0x19,0,0x29,0x65,0x81,0,0xc0];
    #[test] fn descriptor_and_boot_agree() {
        let d = Descriptor::parse(STANDARD).unwrap();
        let bytes = [0x21,0,5,4,4,0x47,0,0];
        assert_eq!(d.decode(&bytes, Physical::default()), boot(&bytes));
        assert_eq!(boot(&[0,0,1,0,0,0,0,0]),Decode::Rollover);
        assert_eq!(d.decode(&[0;7],Physical::default()),Decode::Invalid);
    }
    #[test] fn six_key_selection_retains_held_and_refills() {
        let mut p = Physical::default(); for k in 4..=10 { p.insert(k); }
        let first = p.select(KeyReport::default()); assert_eq!(first.keys,[4,5,6,7,8,9]);
        p.bits[0] &= !(1<<5); assert_eq!(p.select(first).keys,[4,6,7,8,9,10]);
    }
    #[test] fn report_id_bitmap_and_modifiers() {
        let bytes = [5,1,9,6,0xa1,1,0x85,3,5,7,0x19,0xe0,0x29,0xe7,0x75,1,0x95,8,0x81,2,0x19,4,0x29,11,0x95,8,0x81,2,5,8,0x19,1,0x29,5,0x95,5,0x91,2,0x95,3,0x91,1,0xc0];
        let d=Descriptor::parse(&bytes).unwrap();
        let Decode::State(p)=d.decode(&[3,0x80,0x41],Physical::default()) else { panic!() };
        assert_eq!(p.modifiers,0x80); assert!(p.contains(4)&&p.contains(10));
        assert_eq!(d.decode(&[4,0,0],p),Decode::Other);
    }
    #[test] fn array_indices_map_usages_and_release_previous_state() {
        let bytes = [5,1,9,6,0xa1,1,5,7,0x19,4,0x29,11,0x15,0,0x25,7,0x75,8,0x95,1,0x81,0x40,0xc0];
        let d = Descriptor::parse(&bytes).unwrap();
        let Decode::State(p) = d.decode(&[2], Physical::default()) else { panic!() };
        assert!(p.contains(6));
        let Decode::State(released) = d.decode(&[8], p) else { panic!() };
        assert!(released.empty()); // A declared null state releases the array's earlier selection.
    }
    #[test] fn consumer_collection_is_ignored() {
        let bytes = [5,0x0c,9,1,0xa1,1,0x75,8,0x95,1,0x81,2,0xc0];
        assert!(matches!(Descriptor::parse(&bytes), Err("no keyboard collection")));
    }
    #[test] fn descriptor_boundaries() {
        assert!(Descriptor::parse(&STANDARD[..STANDARD.len()-1]).is_err());
        assert!(Descriptor::parse(&[0xfe,0,0]).is_err());
        assert!(Descriptor::parse(&[0x75]).is_err());
    }
}
