// What Juno gives a JIT, measured on the machine rather than read from the kernel source:
//   - CTR_EL0 on every core (A55 and A76 may differ, and Linux traps EL0 reads when they do);
//   - executable memory by mprotect and by a double mapping, and that W^X actually holds;
//   - generated code rewritten at one address and run on this core and on another, with and without cache
//     maintenance, so a stale instruction shows up as a wrong answer;
//   - membarrier, which Cranelift's JIT module and SpiderMonkey use and Juno's kernel does not list;
//   - what Cranelift costs per simulated bytecode on each core, and how large the code is.
// Every test prints one line, so the output is the evidence. Run it on a Juno device: the cores, the traps
// and the W^X behaviour measured here are that machine's.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

const PAGE: usize = 4096;

fn cpu_count() -> usize {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    if unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut set) } != 0 {
        return 1;
    }
    (0..libc::CPU_SETSIZE as usize).filter(|&i| unsafe { libc::CPU_ISSET(i, &set) }).count()
}

fn pin(cpu: usize) -> bool {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_SET(cpu, &mut set) };
    unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set) == 0 }
}

fn current_cpu() -> i32 {
    unsafe { libc::sched_getcpu() }
}

#[cfg(target_arch = "aarch64")]
fn ctr_el0() -> u64 {
    let v: u64;
    unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) v) };
    v
}

#[cfg(not(target_arch = "aarch64"))]
fn ctr_el0() -> u64 {
    0
}

/// Writes code visible to instruction fetch, the sequence from the Arm ARM ("Concurrent modification and execution").
#[cfg(target_arch = "aarch64")]
unsafe fn sync_code(start: *const u8, len: usize) {
    let ctr = ctr_el0();
    let dline = 4usize << ((ctr >> 16) & 0xf);
    let iline = 4usize << (ctr & 0xf);
    let begin = start as usize;
    let end = begin + len;
    unsafe {
        for a in (begin & !(dline - 1)..end).step_by(dline) {
            std::arch::asm!("dc cvau, {}", in(reg) a);
        }
        std::arch::asm!("dsb ish");
        for a in (begin & !(iline - 1)..end).step_by(iline) {
            std::arch::asm!("ic ivau, {}", in(reg) a);
        }
        std::arch::asm!("dsb ish", "isb");
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn sync_code(_start: *const u8, _len: usize) {}

#[inline]
fn local_isb() {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("isb")
    };
}

/// `mov w0, #value; ret` on arm64, or `mov eax, value; ret` on x86-64.
unsafe fn write_return(p: *mut u8, value: u16) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let code = p as *mut u32;
        code.write_volatile(0x5280_0000 | (u32::from(value) << 5));
        code.add(1).write_volatile(0xd65f_03c0);
    }
    #[cfg(not(target_arch = "aarch64"))]
    unsafe {
        p.write_volatile(0xb8);
        (p.add(1) as *mut u32).write_unaligned(u32::from(value));
        p.add(5).write_volatile(0xc3);
    }
}

fn map(prot: i32) -> *mut u8 {
    let p = unsafe { libc::mmap(std::ptr::null_mut(), PAGE, prot, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
    assert_ne!(p, libc::MAP_FAILED, "mmap: {}", std::io::Error::last_os_error());
    p.cast()
}

fn test_ctr() {
    let n = cpu_count();
    let mut seen = vec![];
    for cpu in 0..n {
        if pin(cpu) {
            let c = ctr_el0();
            seen.push(c);
            println!(
                "ctr cpu{cpu} (running on {}) = {c:#x} IminLine={} DminLine={} IDC={} DIC={}",
                current_cpu(),
                4 << (c & 0xf),
                4 << ((c >> 16) & 0xf),
                (c >> 28) & 1,
                (c >> 29) & 1
            );
        } else {
            println!("ctr cpu{cpu}: can't pin: {}", std::io::Error::last_os_error());
        }
    }
    seen.dedup();
    println!("ctr cores agree: {}", seen.len() <= 1);
}

/// A write to a page that has become read-execute must fault, or W^X means nothing.
fn test_wx() {
    let p = map(libc::PROT_READ | libc::PROT_WRITE);
    unsafe {
        write_return(p, 41);
        let r = libc::mprotect(p.cast(), PAGE, libc::PROT_READ | libc::PROT_EXEC);
        println!("mprotect rw->rx: {}", if r == 0 { "ok".to_string() } else { std::io::Error::last_os_error().to_string() });
        sync_code(p, 8);
        let f: extern "C" fn() -> u32 = std::mem::transmute(p);
        println!("run after mprotect: {} (want 41)", f());
        let pid = libc::fork();
        if pid == 0 {
            p.write_volatile(0);
            libc::_exit(0);
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        let faulted = libc::WIFSIGNALED(status);
        println!("write to rx page: {}", if faulted { format!("faulted, signal {} (W^X holds)", libc::WTERMSIG(status)) } else { "SUCCEEDED (W^X broken)".to_string() });

        let rw = map(libc::PROT_READ | libc::PROT_WRITE);
        let pid = libc::fork();
        if pid == 0 {
            let f: extern "C" fn() -> u32 = std::mem::transmute(rw);
            f();
            libc::_exit(0);
        }
        libc::waitpid(pid, &mut status, 0);
        println!(
            "jump to rw page: {}",
            if libc::WIFSIGNALED(status) { format!("faulted, signal {} (no-execute holds)", libc::WTERMSIG(status)) } else { "SUCCEEDED (no-execute broken)".to_string() }
        );
        let wx = libc::mmap(std::ptr::null_mut(), PAGE, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
        println!("rwx mapping: {}", if wx == libc::MAP_FAILED { "refused" } else { "allowed (the kernel does not enforce W^X; it is ours to keep)" });
    }
}

fn test_dual_map() -> Option<(*mut u8, *const u8)> {
    unsafe {
        let fd = libc::syscall(libc::SYS_memfd_create, c"jit".as_ptr(), 0) as i32;
        if fd < 0 {
            println!("memfd_create: {}", std::io::Error::last_os_error());
            return None;
        }
        libc::ftruncate(fd, PAGE as i64);
        let w = libc::mmap(std::ptr::null_mut(), PAGE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        let x = libc::mmap(std::ptr::null_mut(), PAGE, libc::PROT_READ | libc::PROT_EXEC, libc::MAP_SHARED, fd, 0);
        if w == libc::MAP_FAILED || x == libc::MAP_FAILED {
            println!("double mapping: {}", std::io::Error::last_os_error());
            return None;
        }
        write_return(w.cast(), 42);
        sync_code(x.cast(), 8);
        let f: extern "C" fn() -> u32 = std::mem::transmute(x);
        println!("double mapping rw+rx: run gives {} (want 42)", f());
        Some((w.cast(), x.cast()))
    }
}

fn test_membarrier() {
    const QUERY: i32 = 0;
    const REGISTER_SYNC_CORE: i32 = 1 << 6;
    const SYNC_CORE: i32 = 1 << 5;
    unsafe {
        let q = libc::syscall(libc::SYS_membarrier, QUERY, 0, 0);
        println!("membarrier query: {q} ({})", if q < 0 { std::io::Error::last_os_error().to_string() } else { "ok".to_string() });
        let r = libc::syscall(libc::SYS_membarrier, REGISTER_SYNC_CORE, 0, 0);
        let s = libc::syscall(libc::SYS_membarrier, SYNC_CORE, 0, 0);
        println!("membarrier sync-core: register {r}, run {s}");
    }
}

/// Rewrites one address and runs it. `flush` false shows whether maintenance is needed at all on this core.
fn test_rewrite_local(cpu: usize, w: *mut u8, x: *const u8, rounds: u32, flush: bool) {
    pin(cpu);
    let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(x) };
    let mut stale = 0u32;
    for i in 0..rounds {
        let want = (i & 0xffff) as u16;
        unsafe {
            write_return(w, want);
            if flush {
                sync_code(x, 8);
            }
        }
        if f() != u32::from(want) {
            stale += 1;
        }
    }
    println!("rewrite on cpu{cpu} {}: {stale} stale of {rounds}", if flush { "with maintenance" } else { "without maintenance" });
}

static STEP: AtomicU32 = AtomicU32::new(0);
static STALE: AtomicU32 = AtomicU32::new(0);
static CODE: AtomicU64 = AtomicU64::new(0);

/// One core writes and does the maintenance, another runs: code published to a JS thread from a compile thread.
fn test_rewrite_cross(writer: usize, runner: usize, w: *mut u8, x: *const u8, rounds: u32) {
    STEP.store(0, Ordering::SeqCst);
    STALE.store(0, Ordering::SeqCst);
    CODE.store(x as u64, Ordering::SeqCst);
    let r = std::thread::spawn(move || {
        pin(runner);
        let f: extern "C" fn() -> u32 = unsafe { std::mem::transmute(CODE.load(Ordering::SeqCst)) };
        for i in 1..=rounds {
            while STEP.load(Ordering::Acquire) != i * 2 - 1 {
                std::hint::spin_loop();
            }
            // A thread that sees new code runs its own context synchronisation first, as the Arm ARM asks.
            local_isb();
            if f() != (i & 0xffff) {
                STALE.fetch_add(1, Ordering::Relaxed);
            }
            STEP.store(i * 2, Ordering::Release);
        }
    });
    pin(writer);
    for i in 1..=rounds {
        unsafe {
            write_return(w, (i & 0xffff) as u16);
            sync_code(x, 8);
        }
        STEP.store(i * 2 - 1, Ordering::Release);
        while STEP.load(Ordering::Acquire) != i * 2 {
            std::hint::spin_loop();
        }
    }
    r.join().unwrap();
    println!("rewrite cpu{writer} -> run cpu{runner}: {} stale of {rounds}", STALE.load(Ordering::SeqCst));
}

fn test_costs(cpu: usize) {
    pin(cpu);
    let p = map(libc::PROT_READ | libc::PROT_WRITE);
    let n = 2000;
    let t = Instant::now();
    for _ in 0..n {
        unsafe {
            libc::mprotect(p.cast(), PAGE, libc::PROT_READ | libc::PROT_EXEC);
            libc::mprotect(p.cast(), PAGE, libc::PROT_READ | libc::PROT_WRITE);
        }
    }
    println!("mprotect pair on cpu{cpu}: {:.1} us", t.elapsed().as_secs_f64() * 1e6 / f64::from(n));
    let t = Instant::now();
    for _ in 0..n {
        unsafe { sync_code(p, PAGE) };
    }
    println!("cache maintenance of a 4 KiB page on cpu{cpu}: {:.1} us", t.elapsed().as_secs_f64() * 1e6 / f64::from(n));
    let t = Instant::now();
    for _ in 0..n {
        let q = unsafe { libc::mmap(std::ptr::null_mut(), PAGE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
        unsafe {
            (q as *mut u8).write_volatile(1);
            libc::munmap(q, PAGE);
        }
    }
    println!("map, touch, unmap a page on cpu{cpu}: {:.1} us", t.elapsed().as_secs_f64() * 1e6 / f64::from(n));
}

#[cfg(feature = "cranelift")]
mod clif {
    //! A baseline tier emits, per bytecode, what this emits per simulated `Add r1, r2 -> r3`: load two registers from
    //! the VM frame, check both are int32, add with overflow check, box, store, and call a runtime helper otherwise.
    //! That is the shape of Sparkplug's and SpiderMonkey's baseline code, so the time and size are representative.

    use std::time::Instant;

    use cranelift_codegen::ir::condcodes::IntCC;
    use cranelift_codegen::ir::{types, AbiParam, InstBuilder, MemFlagsData, Signature, UserFuncName};
    use cranelift_codegen::isa::CallConv;
    use cranelift_codegen::settings::{self, Configurable};
    use cranelift_codegen::{isa, Context};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

    const INT_TAG: i64 = 0x7ff9_0000_0000_0000u64 as i64;
    const TAG_MASK: i64 = 0xffff_0000_0000_0000u64 as i64;
    const FRAME_REGISTERS: usize = 32;

    pub extern "C" fn helper(fp: *mut u64, op: u64) -> u64 {
        // The slow path of a real tier calls into Rust. Keep it opaque.
        unsafe { (*fp).wrapping_add(op) }
    }

    fn func(ops: usize, cc: CallConv, frontend: cranelift_codegen::isa::TargetFrontendConfig) -> cranelift_codegen::ir::Function {
        let mut sig = Signature::new(cc);
        sig.params.push(AbiParam::new(types::I64));
        sig.params.push(AbiParam::new(types::I64));
        sig.returns.push(AbiParam::new(types::I64));
        let mut f = cranelift_codegen::ir::Function::with_name_signature(UserFuncName::user(0, 0), sig.clone());
        let mut fb = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut f, &mut fb);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        b.seal_block(entry);
        let fp = b.block_params(entry)[0];
        let helper_addr = b.block_params(entry)[1];
        let mut helper_sig = Signature::new(cc);
        helper_sig.params.push(AbiParam::new(types::I64));
        helper_sig.params.push(AbiParam::new(types::I64));
        helper_sig.returns.push(AbiParam::new(types::I64));
        let helper_ref = b.import_signature(helper_sig);
        let flags = MemFlagsData::trusted();
        for i in 0..ops {
            let fast = b.create_block();
            let slow = b.create_block();
            let done = b.create_block();
            b.append_block_param(done, types::I64);
            let a = b.ins().load(types::I64, flags, fp, ((i % FRAME_REGISTERS) * 8) as i32);
            let c = b.ins().load(types::I64, flags, fp, (((i + 1) % FRAME_REGISTERS) * 8) as i32);
            let both = b.ins().band(a, c);
            let tag = b.ins().band_imm(both, TAG_MASK);
            let is_int = b.ins().icmp_imm(IntCC::Equal, tag, INT_TAG);
            b.ins().brif(is_int, fast, &[], slow, &[]);

            b.switch_to_block(fast);
            b.seal_block(fast);
            let x = b.ins().ireduce(types::I32, a);
            let y = b.ins().ireduce(types::I32, c);
            let x = b.ins().sextend(types::I64, x);
            let y = b.ins().sextend(types::I64, y);
            let sum = b.ins().iadd(x, y);
            let narrow = b.ins().ireduce(types::I32, sum);
            let wide = b.ins().sextend(types::I64, narrow);
            let fits = b.ins().icmp(IntCC::Equal, wide, sum);
            let boxed = b.ins().uextend(types::I64, narrow);
            let boxed = b.ins().bor_imm(boxed, INT_TAG);
            let slow2 = b.create_block();
            b.ins().brif(fits, done, &[boxed.into()], slow2, &[]);

            b.switch_to_block(slow2);
            b.seal_block(slow2);
            b.ins().jump(slow, &[]);

            b.switch_to_block(slow);
            b.seal_block(slow);
            let op = b.ins().iconst(types::I64, i as i64);
            let call = b.ins().call_indirect(helper_ref, helper_addr, &[fp, op]);
            let r = b.inst_results(call)[0];
            b.ins().jump(done, &[r.into()]);

            b.switch_to_block(done);
            b.seal_block(done);
            let v = b.block_params(done)[0];
            b.ins().store(flags, v, fp, (((i + 2) % FRAME_REGISTERS) * 8) as i32);
        }
        let result = b.ins().load(types::I64, flags, fp, 0);
        b.ins().return_(&[result]);
        b.finalize(frontend);
        f
    }

    pub fn measure(cpu: usize, regalloc: &str, opt: &str) -> Option<Vec<u8>> {
        super::pin(cpu);
        let mut s = settings::builder();
        s.set("opt_level", opt).unwrap();
        s.set("regalloc_algorithm", regalloc).unwrap();
        s.set("enable_verifier", "false").unwrap();
        s.set("is_pic", "true").unwrap();
        let triple = target_lexicon::Triple::host();
        let isa = isa::lookup(triple).ok()?.finish(settings::Flags::new(s)).ok()?;
        let cc = isa.default_call_conv();
        let mut last = Vec::new();
        for &ops in &[30usize, 300, 3000] {
            let mut total = 0.0;
            let mut size = 0usize;
            let rounds = (3000 / ops).max(3);
            for _ in 0..rounds {
                let f = func(ops, cc, isa.frontend_config());
                let t = Instant::now();
                let mut ctx = Context::for_function(f);
                let mut control = Default::default();
                let code = ctx.compile(&*isa, &mut control).ok()?;
                total += t.elapsed().as_secs_f64();
                size = code.code_buffer().len();
                if ops == 30 {
                    last = code.code_buffer().to_vec();
                }
            }
            println!(
                "cranelift cpu{cpu} {regalloc}/{opt} {ops} ops: {:.2} us/op, {} bytes/op",
                total * 1e6 / (rounds * ops) as f64,
                size / ops
            );
        }
        Some(last)
    }

    /// Runs what Cranelift made for 30 ops, which proves the path from generated bytes to execution.
    pub fn run(code: &[u8]) {
        let p = super::map(libc::PROT_READ | libc::PROT_WRITE);
        assert!(code.len() <= super::PAGE);
        let mut frame = [INT_TAG as u64 | 1; FRAME_REGISTERS];
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
            libc::mprotect(p.cast(), super::PAGE, libc::PROT_READ | libc::PROT_EXEC);
            super::sync_code(p, code.len());
            let f: extern "C" fn(*mut u64, u64) -> u64 = std::mem::transmute(p);
            let r = f(frame.as_mut_ptr(), helper as *const () as u64);
            println!("cranelift code ran: frame[0] = {r:#x}, frame[31] = {:#x}", frame[31]);
        }
    }
}

fn main() {
    let n = cpu_count();
    println!("cpus allowed: {n}");
    test_ctr();
    test_wx();
    test_membarrier();
    let dual = test_dual_map();
    let last = n.saturating_sub(1);
    let cores: Vec<usize> = if n > 1 { vec![0, last] } else { vec![0] };
    if let Some((w, x)) = dual {
        for &c in &cores {
            test_rewrite_local(c, w, x, 200_000, true);
            test_rewrite_local(c, w, x, 200_000, false);
        }
        if n > 1 {
            test_rewrite_cross(0, last, w, x, 100_000);
            test_rewrite_cross(last, 0, w, x, 100_000);
        }
    }
    for &c in &cores {
        test_costs(c);
    }
    #[cfg(feature = "cranelift")]
    for &c in &cores {
        let _ = clif::measure(c, "single_pass", "none");
        let code = clif::measure(c, "backtracking", "none");
        if let Some(code) = code {
            clif::run(&code);
        }
    }
    println!("done");
}
