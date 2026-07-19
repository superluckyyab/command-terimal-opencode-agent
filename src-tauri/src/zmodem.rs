// Native ZMODEM implementation (WindTerm-style): the protocol runs entirely in
// Rust against the PTY byte stream. The engine is fed incoming PTY bytes via
// `feed`; everything it wants written back to the PTY comes out as
// `Event::Write`, terminal-bound bytes as `Event::Forward`, and UI-facing
// notifications as the other variants. Interops with lrzsz's rz/sz.
//
// Protocol reference: the classic zmodem spec (Chuck Forsberg) as implemented
// by lrzsz / TeraTerm.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

pub const ZPAD: u8 = b'*';
pub const ZDLE: u8 = 0x18;
const ZBIN: u8 = b'A';
const ZHEX: u8 = b'B';
const ZBIN32: u8 = b'C';

// frame types
const ZRQINIT: u8 = 0;
const ZRINIT: u8 = 1;
const ZSINIT: u8 = 2;
const ZACK: u8 = 3;
const ZFILE: u8 = 4;
const ZSKIP: u8 = 5;
const ZNAK: u8 = 6;
const ZABORT: u8 = 7;
const ZFIN: u8 = 8;
const ZRPOS: u8 = 9;
const ZDATA: u8 = 10;
const ZEOF: u8 = 11;
const ZFERR: u8 = 12;

// subpacket terminators (sent as ZDLE + this byte)
const ZCRCE: u8 = b'h'; // end of frame, header follows
const ZCRCG: u8 = b'i'; // frame continues, no response expected
const ZCRCQ: u8 = b'j'; // frame continues, ZACK expected
const ZCRCW: u8 = b'k'; // end of frame, ZACK expected
const ZRUB0: u8 = b'l'; // escaped 0x7f
const ZRUB1: u8 = b'm'; // escaped 0xff

// ZRINIT capability flags
const CANFDX: u32 = 0x01;
const CANOVIO: u32 = 0x02;
const CANFC32: u32 = 0x20;
const ESCCTL: u32 = 0x40;

const CAN: u8 = 0x18; // same as ZDLE; 5 in a row = abort

fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn crc32_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

pub enum Event {
    /// bytes that belong to the terminal display
    Forward(Vec<u8>),
    /// bytes to write to the pty
    Write(Vec<u8>),
    /// remote sz offers a file — ask the user where to save it
    RecvOffer { name: String, size: u64 },
    /// remote rz is waiting — ask the user which file(s) to send
    SendReady,
    Progress { name: String, done: u64, total: u64, dir: &'static str },
    Done { name: String, dir: &'static str },
    Error(String),
    /// transfer session fully over — back to plain terminal
    Finished,
}

enum State {
    Idle,
    /// we sent ZRINIT after ZRQINIT; waiting for the ZFILE frame
    RecvWaitFile,
    /// ZFILE parsed, user has not chosen a path yet
    RecvWaitAccept,
    /// receiving ZDATA subpackets into the open file
    Receiving,
    /// got ZEOF, sent ZRINIT; expecting another ZFILE or ZFIN
    RecvWaitNext,
    /// remote rz announced ZRINIT; waiting for the user to pick files
    SendWaitFiles,
    /// sent ZFILE; waiting for ZRPOS to start data
    SendWaitPos,
    /// streaming ZDATA subpackets (pumped)
    Streaming,
    /// sent ZEOF; waiting for the receiver's ZRINIT
    SendWaitEofAck,
    /// sent ZFIN; waiting for peer ZFIN
    FinWait,
}

struct Header {
    enc: u8, // ZBIN / ZHEX / ZBIN32
    typ: u8,
    data: [u8; 4],
}

impl Header {
    fn pos(&self) -> u32 {
        // ZDATA/ZRPOS/ZEOF/ZACK carry a little-endian position
        u32::from_le_bytes(self.data)
    }
    fn flags(&self) -> u32 {
        // ZRINIT capability flags live in the last data byte(s), big-endian order F3..F0
        u32::from_be_bytes(self.data)
    }
}

pub struct Zm {
    state: State,
    buf: Vec<u8>,
    crc32t: [u32; 256],
    // receive side
    recv_file: Option<File>,
    recv_name: String,
    recv_size: u64,
    recv_pos: u64,
    // subpacket decode state
    sp_data: Vec<u8>,
    sp_esc: bool,
    sp_end: Option<u8>,
    sp_crc: Vec<u8>,
    in_data_frame: bool,
    pending_zfile: bool,
    // send side
    peer_flags: u32,
    files: Vec<PathBuf>,
    file_idx: usize,
    send_file: Option<File>,
    send_name: String,
    send_size: u64,
    send_pos: u64,
    can_count: u8,
}

impl Zm {
    pub fn new() -> Self {
        Zm {
            state: State::Idle,
            buf: Vec::new(),
            crc32t: crc32_table(),
            recv_file: None,
            recv_name: String::new(),
            recv_size: 0,
            recv_pos: 0,
            sp_data: Vec::new(),
            sp_esc: false,
            sp_end: None,
            sp_crc: Vec::new(),
            in_data_frame: false,
            pending_zfile: false,
            peer_flags: 0,
            files: Vec::new(),
            file_idx: 0,
            send_file: None,
            send_name: String::new(),
            send_size: 0,
            send_pos: 0,
            can_count: 0,
        }
    }

    pub fn active(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    fn use_crc32(&self) -> bool {
        self.peer_flags & CANFC32 != 0
    }

    // ── encoding helpers ──

    fn hex_header(&self, typ: u8, data: [u8; 4]) -> Vec<u8> {
        let mut five = [0u8; 5];
        five[0] = typ;
        five[1..5].copy_from_slice(&data);
        let crc = crc16(&five);
        let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
        for b in five.iter().chain(crc.to_be_bytes().iter()) {
            out.push(b"0123456789abcdef"[(b >> 4) as usize]);
            out.push(b"0123456789abcdef"[(b & 0xf) as usize]);
        }
        out.extend_from_slice(b"\r\n");
        // XON after every hex header except ZACK and ZFIN
        if typ != ZACK && typ != ZFIN {
            out.push(0x11);
        }
        out
    }

    fn esc_byte(&self, b: u8, last: u8, out: &mut Vec<u8>) {
        let ctl_esc = self.peer_flags & ESCCTL != 0;
        let must = matches!(b, 0x10 | 0x90 | 0x11 | 0x91 | 0x13 | 0x93 | 0x18)
            || (b & 0x7f == 0x0d && last & 0x7f == b'@')
            || (ctl_esc && b & 0x60 == 0);
        if must {
            out.push(ZDLE);
            out.push(b ^ 0x40);
        } else {
            out.push(b);
        }
    }

    fn bin_header(&self, typ: u8, data: [u8; 4]) -> Vec<u8> {
        let crc32 = self.use_crc32();
        let mut out = vec![ZPAD, ZDLE, if crc32 { ZBIN32 } else { ZBIN }];
        let mut five = Vec::with_capacity(9);
        five.push(typ);
        five.extend_from_slice(&data);
        if crc32 {
            let c = self.crc32(&five);
            five.extend_from_slice(&c.to_le_bytes());
        } else {
            let c = crc16(&five);
            five.extend_from_slice(&c.to_be_bytes());
        }
        let mut last = 0u8;
        for b in five {
            self.esc_byte(b, last, &mut out);
            last = b;
        }
        out
    }

    fn subpacket(&self, data: &[u8], end: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 16);
        let mut last = 0u8;
        for &b in data {
            self.esc_byte(b, last, &mut out);
            last = b;
        }
        out.push(ZDLE);
        out.push(end);
        if self.use_crc32() {
            let mut c: u32 = 0xffff_ffff;
            for &b in data {
                c = self.crc32t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
            }
            c = self.crc32t[((c ^ end as u32) & 0xff) as usize] ^ (c >> 8);
            c ^= 0xffff_ffff;
            let mut l = end;
            for b in c.to_le_bytes() {
                self.esc_byte(b, l, &mut out);
                l = b;
            }
        } else {
            let mut five = Vec::with_capacity(data.len() + 1);
            five.extend_from_slice(data);
            five.push(end);
            let c = crc16(&five);
            let mut l = end;
            for b in c.to_be_bytes() {
                self.esc_byte(b, l, &mut out);
                l = b;
            }
        }
        out
    }

    fn crc32(&self, data: &[u8]) -> u32 {
        let mut c: u32 = 0xffff_ffff;
        for &b in data {
            c = self.crc32t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
        }
        c ^ 0xffff_ffff
    }

    // ── public API ──

    /// User picked a save path for the offered file.
    pub fn accept_receive(&mut self, path: PathBuf, out: &mut Vec<Event>) {
        if !matches!(self.state, State::RecvWaitAccept) {
            return;
        }
        match File::create(&path) {
            Ok(f) => {
                self.recv_file = Some(f);
                self.recv_pos = 0;
                self.state = State::Receiving;
                let w = self.hex_header(ZRPOS, 0u32.to_le_bytes());
                out.push(Event::Write(w));
            }
            Err(e) => {
                out.push(Event::Error(format!("cannot create {}: {}", path.display(), e)));
                let w = self.hex_header(ZSKIP, [0; 4]);
                out.push(Event::Write(w));
                self.state = State::RecvWaitNext;
            }
        }
    }

    /// User picked local files to upload to the remote rz.
    pub fn start_send(&mut self, paths: Vec<PathBuf>, out: &mut Vec<Event>) {
        if !matches!(self.state, State::SendWaitFiles) || paths.is_empty() {
            return;
        }
        self.files = paths;
        self.file_idx = 0;
        self.send_next_file(out);
    }

    /// Abort whatever is in flight.
    pub fn cancel(&mut self, out: &mut Vec<Event>) {
        if self.active() {
            out.push(Event::Write(vec![CAN, CAN, CAN, CAN, CAN, 8, 8, 8, 8, 8]));
            out.push(Event::Error("transfer cancelled".into()));
            self.reset(out);
        }
    }

    fn reset(&mut self, out: &mut Vec<Event>) {
        self.state = State::Idle;
        self.recv_file = None;
        self.send_file = None;
        self.files.clear();
        self.in_data_frame = false;
        self.sp_reset();
        out.push(Event::Finished);
    }

    fn sp_reset(&mut self) {
        self.sp_data.clear();
        self.sp_esc = false;
        self.sp_end = None;
        self.sp_crc.clear();
    }

    fn send_next_file(&mut self, out: &mut Vec<Event>) {
        while self.file_idx < self.files.len() {
            let p = self.files[self.file_idx].clone();
            match File::open(&p) {
                Ok(f) => {
                    self.send_size = f.metadata().map(|m| m.len()).unwrap_or(0);
                    self.send_name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "file".into());
                    self.send_file = Some(f);
                    self.send_pos = 0;
                    let mut sp = Vec::new();
                    sp.extend_from_slice(self.send_name.as_bytes());
                    sp.push(0);
                    sp.extend_from_slice(self.send_size.to_string().as_bytes());
                    let mut w = self.bin_header(ZFILE, [0; 4]);
                    w.extend_from_slice(&self.subpacket(&sp, ZCRCW));
                    out.push(Event::Write(w));
                    self.state = State::SendWaitPos;
                    return;
                }
                Err(e) => {
                    out.push(Event::Error(format!("cannot open {}: {}", p.display(), e)));
                    self.file_idx += 1;
                }
            }
        }
        // no more files → ZFIN
        let w = self.hex_header(ZFIN, [0; 4]);
        out.push(Event::Write(w));
        self.state = State::FinWait;
    }

    /// While `Streaming`, emit the next chunk of ZDATA subpackets. Call in a
    /// loop (with the writes applied between calls) until it returns false.
    pub fn pump(&mut self, out: &mut Vec<Event>) -> bool {
        if !matches!(self.state, State::Streaming) {
            return false;
        }
        if self.send_file.is_none() {
            return false;
        }
        // Read the payload (mutable borrow of the file) into owned chunks FIRST,
        // then encode (immutable borrow of self) — the two borrows can't overlap.
        let mut chunk = [0u8; 1024];
        let mut pieces: Vec<(Vec<u8>, u8)> = Vec::new();
        for _ in 0..8 {
            let n = {
                let f = self.send_file.as_mut().unwrap();
                f.read(&mut chunk).unwrap_or(0)
            };
            if n == 0 {
                break;
            }
            self.send_pos += n as u64;
            let end = if self.send_pos >= self.send_size { ZCRCE } else { ZCRCG };
            pieces.push((chunk[..n].to_vec(), end));
            if end == ZCRCE {
                break;
            }
        }
        let mut burst = Vec::new();
        for (data, end) in &pieces {
            burst.extend_from_slice(&self.subpacket(data, *end));
        }
        let finished = self.send_pos >= self.send_size;
        if !burst.is_empty() {
            out.push(Event::Write(burst));
        }
        out.push(Event::Progress {
            name: self.send_name.clone(),
            done: self.send_pos,
            total: self.send_size,
            dir: "up",
        });
        if finished {
            let w = self.hex_header(ZEOF, (self.send_pos as u32).to_le_bytes());
            out.push(Event::Write(w));
            self.state = State::SendWaitEofAck;
            return false;
        }
        true
    }

    pub fn needs_pump(&self) -> bool {
        matches!(self.state, State::Streaming)
    }

    // ── incoming bytes ──

    pub fn feed(&mut self, data: &[u8], out: &mut Vec<Event>) {
        self.buf.extend_from_slice(data);
        loop {
            let before = self.buf.len();
            self.step(out);
            if self.buf.len() == before {
                break;
            }
        }
        // don't let junk accumulate forever while idle
        if matches!(self.state, State::Idle) && self.buf.len() > 8192 {
            let drain: Vec<u8> = self.buf.drain(..self.buf.len() - 64).collect();
            out.push(Event::Forward(drain));
        }
    }

    fn step(&mut self, out: &mut Vec<Event>) {
        if self.pending_zfile {
            self.pending_zfile_check(out);
            if self.pending_zfile {
                return; // still waiting for the rest of the name subpacket
            }
        }
        match self.state {
            State::Idle => self.step_idle(out),
            State::Receiving if self.in_data_frame => self.step_data(out),
            _ => self.step_frame(out),
        }
    }

    /// Idle: forward everything up to a possible ZMODEM signature; parse a hex
    /// header when one fully arrives.
    fn step_idle(&mut self, out: &mut Vec<Event>) {
        // find "**<ZDLE>B"
        let sig = [ZPAD, ZPAD, ZDLE, ZHEX];
        let pos = self
            .buf
            .windows(4)
            .position(|w| w == sig);
        match pos {
            None => {
                // keep a small tail in case the signature is split across chunks
                if self.buf.len() > 3 {
                    let keep = self.buf.split_off(self.buf.len() - 3);
                    let fwd = std::mem::replace(&mut self.buf, keep);
                    out.push(Event::Forward(fwd));
                }
            }
            Some(p) => {
                if p > 0 {
                    let rest = self.buf.split_off(p);
                    let fwd = std::mem::replace(&mut self.buf, rest);
                    out.push(Event::Forward(fwd));
                }
                // need 4 + 14 hex digits
                if self.buf.len() < 18 {
                    return;
                }
                let hex = &self.buf[4..18];
                match parse_hex(hex) {
                    Some(five) if crc16(&five[..5]) == u16::from_be_bytes([five[5], five[6]]) => {
                        let h = Header { enc: ZHEX, typ: five[0], data: [five[1], five[2], five[3], five[4]] };
                        self.buf.drain(..18);
                        self.eat_eol();
                        self.on_header(h, out);
                    }
                    _ => {
                        // not a valid header — forward the '*' and move on
                        let b = self.buf.remove(0);
                        out.push(Event::Forward(vec![b]));
                    }
                }
            }
        }
    }

    fn eat_eol(&mut self) {
        while let Some(&b) = self.buf.first() {
            if b == 0x0d || b == 0x0a || b == 0x8d || b == 0x8a || b == 0x11 || b == 0x13 {
                self.buf.remove(0);
            } else {
                break;
            }
        }
    }

    /// In-transfer: parse the next frame header (hex or binary).
    fn step_frame(&mut self, out: &mut Vec<Event>) {
        // drop line noise until a ZPAD
        while let Some(&b) = self.buf.first() {
            if b == ZPAD {
                break;
            }
            if b == CAN {
                self.can_count += 1;
                if self.can_count >= 5 {
                    out.push(Event::Error("remote aborted the transfer".into()));
                    self.reset(out);
                    self.can_count = 0;
                    return;
                }
            } else {
                self.can_count = 0;
            }
            self.buf.remove(0);
        }
        if self.buf.len() < 3 {
            return;
        }
        // hex: * * ZDLE B …    binary: * ZDLE A/C …
        if self.buf[1] == ZPAD {
            if self.buf.len() < 4 || self.buf[2] != ZDLE {
                self.buf.remove(0);
                return;
            }
            if self.buf[3] != ZHEX {
                self.buf.remove(0);
                return;
            }
            if self.buf.len() < 18 {
                return;
            }
            match parse_hex(&self.buf[4..18]) {
                Some(five) if crc16(&five[..5]) == u16::from_be_bytes([five[5], five[6]]) => {
                    let h = Header { enc: ZHEX, typ: five[0], data: [five[1], five[2], five[3], five[4]] };
                    self.buf.drain(..18);
                    self.eat_eol();
                    self.on_header(h, out);
                }
                _ => {
                    self.buf.remove(0);
                }
            }
            return;
        }
        if self.buf[1] != ZDLE {
            self.buf.remove(0);
            return;
        }
        let enc = self.buf[2];
        if enc != ZBIN && enc != ZBIN32 {
            self.buf.remove(0);
            return;
        }
        // unescape 5 + crc bytes
        let need = if enc == ZBIN32 { 9 } else { 7 };
        let mut vals = Vec::with_capacity(need);
        let mut i = 3;
        while vals.len() < need && i < self.buf.len() {
            let b = self.buf[i];
            if b == ZDLE {
                if i + 1 >= self.buf.len() {
                    return; // wait for more
                }
                i += 1;
                let e = self.buf[i];
                vals.push(match e {
                    ZRUB0 => 0x7f,
                    ZRUB1 => 0xff,
                    _ => e ^ 0x40,
                });
            } else if b == 0x11 || b == 0x13 || b == 0x91 || b == 0x93 {
                // swallow flow control noise
            } else {
                vals.push(b);
            }
            i += 1;
        }
        if vals.len() < need {
            return;
        }
        let ok = if enc == ZBIN32 {
            let c = self.crc32(&vals[..5]);
            c.to_le_bytes() == [vals[5], vals[6], vals[7], vals[8]]
        } else {
            crc16(&vals[..5]) == u16::from_be_bytes([vals[5], vals[6]])
        };
        self.buf.drain(..i);
        if !ok {
            let w = self.hex_header(ZNAK, [0; 4]);
            out.push(Event::Write(w));
            return;
        }
        let h = Header { enc, typ: vals[0], data: [vals[1], vals[2], vals[3], vals[4]] };
        self.on_header(h, out);
    }

    /// Receiving: decode escaped ZDATA subpackets and write them to disk.
    fn step_data(&mut self, out: &mut Vec<Event>) {
        let crc_len = if self.use_crc32() { 4 } else { 2 };
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            if let Some(_end) = self.sp_end {
                // collecting CRC bytes (also escaped)
                if self.sp_esc {
                    self.sp_esc = false;
                    self.sp_crc.push(match b {
                        ZRUB0 => 0x7f,
                        ZRUB1 => 0xff,
                        _ => b ^ 0x40,
                    });
                } else if b == ZDLE {
                    self.sp_esc = true;
                } else if b == 0x11 || b == 0x13 || b == 0x91 || b == 0x93 {
                    // flow control noise
                } else {
                    self.sp_crc.push(b);
                }
                i += 1;
                if self.sp_crc.len() >= crc_len {
                    self.buf.drain(..i);
                    self.finish_subpacket(out);
                    return;
                }
                continue;
            }
            if self.sp_esc {
                self.sp_esc = false;
                match b {
                    ZCRCE | ZCRCG | ZCRCQ | ZCRCW => {
                        self.sp_end = Some(b);
                    }
                    ZRUB0 => self.sp_data.push(0x7f),
                    ZRUB1 => self.sp_data.push(0xff),
                    CAN => {
                        // ZDLE ZDLE… count towards abort
                        self.can_count += 1;
                        if self.can_count >= 4 {
                            out.push(Event::Error("remote aborted the transfer".into()));
                            self.buf.drain(..=i);
                            self.reset(out);
                            return;
                        }
                        self.sp_esc = true; // treat like a fresh ZDLE
                    }
                    _ => self.sp_data.push(b ^ 0x40),
                }
            } else if b == ZDLE {
                self.sp_esc = true;
            } else if b == 0x11 || b == 0x13 || b == 0x91 || b == 0x93 {
                // strip XON/XOFF noise
            } else {
                self.can_count = 0;
                self.sp_data.push(b);
            }
            i += 1;
        }
        self.buf.clear();
    }

    fn finish_subpacket(&mut self, out: &mut Vec<Event>) {
        let end = self.sp_end.unwrap_or(ZCRCE);
        let ok = if self.use_crc32() {
            let mut c: u32 = 0xffff_ffff;
            for &b in &self.sp_data {
                c = self.crc32t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
            }
            c = self.crc32t[((c ^ end as u32) & 0xff) as usize] ^ (c >> 8);
            c ^= 0xffff_ffff;
            self.sp_crc.as_slice() == c.to_le_bytes()
        } else {
            let mut v = self.sp_data.clone();
            v.push(end);
            let c = crc16(&v);
            self.sp_crc.as_slice() == c.to_be_bytes()
        };
        if !ok {
            self.sp_reset();
            self.in_data_frame = false;
            let w = self.hex_header(ZRPOS, (self.recv_pos as u32).to_le_bytes());
            out.push(Event::Write(w));
            return;
        }
        if let Some(f) = self.recv_file.as_mut() {
            let _ = f.write_all(&self.sp_data);
        }
        self.recv_pos += self.sp_data.len() as u64;
        out.push(Event::Progress {
            name: self.recv_name.clone(),
            done: self.recv_pos,
            total: self.recv_size,
            dir: "down",
        });
        let ack = matches!(end, ZCRCQ | ZCRCW);
        let frame_ends = matches!(end, ZCRCE | ZCRCW);
        self.sp_reset();
        if ack {
            let w = self.hex_header(ZACK, (self.recv_pos as u32).to_le_bytes());
            out.push(Event::Write(w));
        }
        if frame_ends {
            self.in_data_frame = false; // next: a header (ZDATA cont. or ZEOF)
        }
    }

    fn on_header(&mut self, h: Header, out: &mut Vec<Event>) {
        match h.typ {
            ZRQINIT => {
                // remote sz wants to send us files
                self.peer_flags = 0;
                self.state = State::RecvWaitFile;
                let flags = (CANFDX | CANOVIO | CANFC32) as u32;
                let w = self.hex_header(ZRINIT, flags.to_be_bytes());
                out.push(Event::Write(w));
            }
            ZRINIT => match self.state {
                State::Idle | State::SendWaitFiles => {
                    // remote rz is ready to receive
                    self.peer_flags = h.flags();
                    if matches!(self.state, State::Idle) {
                        self.state = State::SendWaitFiles;
                        out.push(Event::SendReady);
                    }
                }
                State::SendWaitEofAck => {
                    self.peer_flags = h.flags();
                    out.push(Event::Done { name: self.send_name.clone(), dir: "up" });
                    self.send_file = None;
                    self.file_idx += 1;
                    self.send_next_file(out);
                }
                State::SendWaitPos | State::Streaming => {
                    self.peer_flags = h.flags();
                }
                _ => {}
            },
            ZSINIT => {
                // accept (empty) attn string subpacket lazily; just ACK
                let w = self.hex_header(ZACK, [0; 4]);
                out.push(Event::Write(w));
            }
            ZFILE => {
                // filename subpacket follows — reuse the data decoder inline
                self.in_data_frame = false;
                if let Some(info) = self.take_file_subpacket() {
                    let (name, size) = info;
                    self.recv_name = name.clone();
                    self.recv_size = size;
                    self.state = State::RecvWaitAccept;
                    out.push(Event::RecvOffer { name, size });
                } else {
                    // subpacket not complete yet — keep header pending by
                    // pushing a marker back; simplest: stash via state
                    self.state = State::RecvWaitFile;
                    self.pending_zfile = true;
                }
            }
            ZDATA => {
                if matches!(self.state, State::Receiving) {
                    let pos = h.pos() as u64;
                    if pos != self.recv_pos {
                        let w = self.hex_header(ZRPOS, (self.recv_pos as u32).to_le_bytes());
                        out.push(Event::Write(w));
                    } else {
                        self.in_data_frame = true;
                        self.sp_reset();
                    }
                }
            }
            ZEOF => {
                if matches!(self.state, State::Receiving) {
                    if h.pos() as u64 == self.recv_pos {
                        if let Some(f) = self.recv_file.take() {
                            let _ = f.sync_all();
                        }
                        out.push(Event::Done { name: self.recv_name.clone(), dir: "down" });
                        self.state = State::RecvWaitNext;
                        let flags = (CANFDX | CANOVIO | CANFC32) as u32;
                        let w = self.hex_header(ZRINIT, flags.to_be_bytes());
                        out.push(Event::Write(w));
                    } else {
                        let w = self.hex_header(ZRPOS, (self.recv_pos as u32).to_le_bytes());
                        out.push(Event::Write(w));
                    }
                }
            }
            ZRPOS => match self.state {
                State::SendWaitPos | State::Streaming | State::SendWaitEofAck => {
                    let pos = h.pos() as u64;
                    if let Some(f) = self.send_file.as_mut() {
                        let _ = f.seek(SeekFrom::Start(pos));
                        self.send_pos = pos;
                        let w = self.bin_header(ZDATA, (pos as u32).to_le_bytes());
                        out.push(Event::Write(w));
                        self.state = State::Streaming;
                    }
                }
                _ => {}
            },
            ZSKIP => {
                if matches!(self.state, State::SendWaitPos | State::Streaming) {
                    out.push(Event::Error(format!("remote skipped {}", self.send_name)));
                    self.send_file = None;
                    self.file_idx += 1;
                    self.send_next_file(out);
                }
            }
            ZACK => {}
            ZNAK => {
                // resend whatever we last offered
                if matches!(self.state, State::SendWaitPos) {
                    self.file_idx = self.file_idx.min(self.files.len());
                    let idx = self.file_idx;
                    if idx < self.files.len() {
                        // re-send the ZFILE frame
                        self.send_next_file(out);
                    }
                }
            }
            ZFIN => match self.state {
                State::FinWait => {
                    out.push(Event::Write(b"OO".to_vec()));
                    self.reset(out);
                }
                State::RecvWaitNext | State::RecvWaitFile => {
                    let w = self.hex_header(ZFIN, [0; 4]);
                    out.push(Event::Write(w));
                    // sender answers with "OO" which we just swallow
                    self.reset(out);
                }
                _ => {}
            },
            ZABORT | ZFERR => {
                out.push(Event::Error("remote aborted the transfer".into()));
                let w = self.hex_header(ZFIN, [0; 4]);
                out.push(Event::Write(w));
                self.reset(out);
            }
            _ => {}
        }
    }

    /// Try to decode the ZCRCW subpacket that follows a ZFILE header.
    /// Returns (name, size) once the whole subpacket is in the buffer.
    fn take_file_subpacket(&mut self) -> Option<(String, u64)> {
        // decode without consuming unless complete
        let crc_len = if self.use_crc32() { 4 } else { 2 };
        let mut data = Vec::new();
        let mut crc = Vec::new();
        let mut end: Option<u8> = None;
        let mut esc = false;
        let mut used = 0usize;
        for (idx, &b) in self.buf.iter().enumerate() {
            if end.is_some() {
                if esc {
                    esc = false;
                    crc.push(match b {
                        ZRUB0 => 0x7f,
                        ZRUB1 => 0xff,
                        _ => b ^ 0x40,
                    });
                } else if b == ZDLE {
                    esc = true;
                } else if b == 0x11 || b == 0x13 || b == 0x91 || b == 0x93 {
                } else {
                    crc.push(b);
                }
                if crc.len() >= crc_len {
                    used = idx + 1;
                    break;
                }
                continue;
            }
            if esc {
                esc = false;
                match b {
                    ZCRCE | ZCRCG | ZCRCQ | ZCRCW => end = Some(b),
                    ZRUB0 => data.push(0x7f),
                    ZRUB1 => data.push(0xff),
                    _ => data.push(b ^ 0x40),
                }
            } else if b == ZDLE {
                esc = true;
            } else if b == 0x11 || b == 0x13 || b == 0x91 || b == 0x93 {
            } else {
                data.push(b);
            }
        }
        if used == 0 {
            return None;
        }
        let e = end.unwrap_or(ZCRCW);
        let ok = if self.use_crc32() {
            let mut c: u32 = 0xffff_ffff;
            for &b in &data {
                c = self.crc32t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
            }
            c = self.crc32t[((c ^ e as u32) & 0xff) as usize] ^ (c >> 8);
            c ^= 0xffff_ffff;
            crc.as_slice() == c.to_le_bytes()
        } else {
            let mut v = data.clone();
            v.push(e);
            crc16(&v).to_be_bytes().as_slice() == crc.as_slice()
        };
        if !ok {
            // drop the bad bytes; caller will ZNAK via retry from sender
            self.buf.drain(..used);
            return None;
        }
        self.buf.drain(..used);
        // "name\0size mtime mode …"
        let nul = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        let name = String::from_utf8_lossy(&data[..nul]).into_owned();
        let name = name.rsplit(['/', '\\']).next().unwrap_or("file").to_string();
        let rest = if nul + 1 < data.len() { &data[nul + 1..] } else { &[][..] };
        let size = String::from_utf8_lossy(rest)
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Some((name, size))
    }
}

// A ZFILE header can arrive before its name subpacket; remember that.
impl Zm {
    fn pending_zfile_check(&mut self, out: &mut Vec<Event>) {
        if self.pending_zfile {
            if let Some((name, size)) = self.take_file_subpacket() {
                self.pending_zfile = false;
                self.recv_name = name.clone();
                self.recv_size = size;
                self.state = State::RecvWaitAccept;
                out.push(Event::RecvOffer { name, size });
            }
        }
    }
}

fn parse_hex(h: &[u8]) -> Option<[u8; 7]> {
    let mut out = [0u8; 7];
    if h.len() < 14 {
        return None;
    }
    for i in 0..7 {
        let hi = hex_val(h[i * 2])?;
        let lo = hex_val(h[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
