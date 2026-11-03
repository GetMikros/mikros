"""GDB commands for a Mikros kernel attached through QEMU's GDB stub.

Load the kernel's symbols, then this file with `source`. It adds:

    mikros ps                 every process
    mikros threads PROCESS    the threads of a process, with their state,
                              priority and saved registers
    mikros handles PROCESS    the handle table of a process
    mikros mappings PROCESS   the mappings of a process

where PROCESS is a process's object ID or name.
    mikros log [COUNT]        the newest records of the kernel log ring

The commands read the kernel's data structures through its debug
information and use the Rust toolchain's own GDB helpers for the layout
of standard library types.
"""

import os
import re
import subprocess
import sys

import gdb


def _rust_helpers():
    """The `gdb_providers` module of the pinned Rust toolchain."""
    try:
        import gdb_providers
    except ImportError:
        here = os.path.dirname(os.path.abspath(__file__))
        sysroot = subprocess.run(
            ["rustc", "--print", "sysroot"],
            cwd=here,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        sys.path.append(os.path.join(sysroot, "lib", "rustlib", "etc"))
        import gdb_providers
    return gdb_providers


rust = _rust_helpers()

PROCESS_STATES = ["NEW", "RUNNING", "TERMINATED", "STOPPED"]
THREAD_STATES = ["NEW", "RUNNABLE", "BLOCKED", "EXCEPTION", "DEAD", "SUSPENDED"]
SEVERITIES = "DIWE"
PERMS = [(1, "r"), (2, "w"), (4, "x")]
# The general registers a thread's kernel stack holds while it is switched
# out, lowest address first, as `context_switch` pushes them.
SWITCH_FRAME = ["r15", "r14", "r13", "r12", "rbx", "rbp", "rip"]


def static(name):
    symbol = gdb.lookup_static_symbol(name)
    if symbol is None:
        raise gdb.GdbError(f"{name} not found; are the kernel's symbols loaded?")
    return symbol.value()


def needs_kernel(invoke):
    """Wraps a command's `invoke` so that it reads memory through the boot
    processor, the only one the kernel runs on, and fails with a short
    message when the kernel's memory cannot be read yet, as before the
    boot loader has started the kernel.

    QEMU's GDB stub reads memory through the page tables of the selected
    processor. Ctrl-C can leave another processor selected, still running
    the firmware, where no kernel address is mapped."""

    def wrapped(self, arg, from_tty):
        selected = gdb.selected_thread()
        boot = min(gdb.selected_inferior().threads(), key=lambda t: t.num, default=None)
        if boot is not None and boot != selected:
            boot.switch()
        try:
            int(scalar(static("kernel::process::FIRST_PROCESS")))
            return invoke(self, arg, from_tty)
        except gdb.MemoryError as e:
            raise gdb.GdbError(
                f"{e}\nThe kernel is not running yet. Let it boot first, "
                "for example with `hbreak _start` and `continue`, then "
                "interrupt it with Ctrl-C."
            )
        finally:
            if selected is not None and selected.is_valid() and selected != gdb.selected_thread():
                selected.switch()

    return wrapped


def scalar(value):
    """The number or pointer inside atomics and other wrappers."""
    return rust.unwrap_scalar_wrappers(value)


def word(address):
    memory = gdb.selected_inferior().read_memory(address, 8)
    return int.from_bytes(bytes(memory), "little")


def locked(lock):
    """The value a `SpinLock` holds, read without taking it."""
    return lock["value"]["value"]


def arc_inner(pointer):
    return rust.unwrap_non_null_or_wrapper(pointer["ptr"]).dereference()


def vec_items(vec):
    element = vec.type.strip_typedefs().template_argument(0)
    data = rust.unwrap_non_null_or_wrapper(vec["buf"]["inner"]["ptr"])
    data = data.reinterpret_cast(element.pointer())
    return [(data + i).dereference() for i in range(int(vec["len"]))]


def text(name):
    raw = name["bytes"]
    return bytes(int(raw[i]) for i in range(int(name["len"]))).decode(errors="replace")


def perms(bits):
    return "".join(c if bits & b else "-" for b, c in PERMS)


def processes():
    """Every process, newest first."""
    pointer = scalar(static("kernel::process::FIRST_PROCESS"))
    while int(pointer) != 0:
        process = pointer.dereference()
        yield process
        pointer = scalar(process["next"])


def process(arg):
    """The process with the object ID or the name `arg`, the newest one
    with that name."""
    arg = arg.strip()
    if not arg:
        raise gdb.GdbError("expected a process's object ID or name")
    for p in processes():
        if str(int(p["header"]["koid"])) == arg or text(p["name"]) == arg:
            return p
    raise gdb.GdbError(f"no process {arg}")


def threads(process):
    """The threads of `process` that still exist, with the addresses of
    their `ArcInner`."""
    for weak in vec_items(locked(process["threads"])):
        inner = rust.unwrap_non_null_or_wrapper(weak["ptr"])
        if int(inner) == (1 << 64) - 1 or int(scalar(inner.dereference()["strong"])) == 0:
            continue
        yield int(inner), inner.dereference()["data"]


def running_thread():
    """The address of the `ArcInner` of the thread running on the boot
    processor, or 0."""
    sched = locked(static("kernel::percpu::BOOT_CPU")["sched"])
    return word(int(sched["current"].address))


_vtables = None


def object_types():
    """The type names of objects by the addresses of their `Object`
    vtables, of which each codegen unit may have its own copy."""
    global _vtables
    if _vtables is None:
        _vtables = {}
        listing = gdb.execute(
            r"info variables kernel_core::object::Object>::.vtable", to_string=True
        )
        for name in set(re.findall(r"<(.+?) as kernel_core::object::Object>", listing)):
            symbol = f"<{name} as kernel_core::object::Object>::{{vtable}}"
            for s in gdb.lookup_static_symbols(symbol):
                _vtables[int(s.value().address)] = name
    return _vtables


def count_argument(arg):
    try:
        return int(gdb.parse_and_eval(arg))
    except gdb.error as e:
        raise gdb.GdbError(f"expected a count: {e}")


def object_of(entry_address):
    """The concrete type name and object ID of the `Arc<dyn Object>` at
    `entry_address`, found through its vtable."""
    data, vtable = word(entry_address), word(entry_address + 8)
    name = object_types().get(vtable)
    if name is None:
        return "?", 0
    # The object follows the two counts of its `ArcInner`, at its own
    # alignment, which the vtable holds after the drop function and size.
    align = word(vtable + 16)
    offset = (16 + align - 1) // align * align
    try:
        concrete = gdb.lookup_type(name)
    except gdb.error:
        return name, 0
    value = gdb.Value(data + offset).cast(concrete.pointer()).dereference()
    return name.rsplit("::", 1)[-1], int(value["header"]["koid"])


class Mikros(gdb.Command):
    """Commands for inspecting a Mikros kernel."""

    def __init__(self):
        super().__init__("mikros", gdb.COMMAND_USER, prefix=True)


class Ps(gdb.Command):
    """List every process: mikros ps"""

    def __init__(self):
        super().__init__("mikros ps", gdb.COMMAND_USER)

    @needs_kernel
    def invoke(self, arg, from_tty):
        print(f"{'KOID':>6}  {'STATE':<10}  {'THREADS':>7}  NAME")
        for p in processes():
            live = sum(1 for _, t in threads(p) if int(scalar(t["state"])) != 4)
            state = PROCESS_STATES[int(scalar(p["state"]))]
            print(f"{int(p['header']['koid']):>6}  {state:<10}  {live:>7}  {text(p['name'])}")


class Threads(gdb.Command):
    """List the threads of a process with their saved registers:
    mikros threads PROCESS"""

    def __init__(self):
        super().__init__("mikros threads", gdb.COMMAND_USER)

    @needs_kernel
    def invoke(self, arg, from_tty):
        running = running_thread()
        for inner, t in threads(process(arg)):
            state = THREAD_STATES[int(scalar(t["state"]))]
            print(
                f"thread {int(t['header']['koid'])} {text(t['name'])!r}: {state}, "
                f"priority {int(scalar(t['priority']))}"
            )
            rsp = int(t["rsp"]["value"])
            if inner == running:
                print("  running; its registers are GDB's")
            elif state == "DEAD" or int(scalar(t["stack_top"])) == 0:
                # A dead thread's kernel stack may be freed already.
                continue
            elif rsp == 0:
                print("  never switched to")
            else:
                saved = [word(rsp + 8 * i) for i in range(len(SWITCH_FRAME))]
                regs = " ".join(f"{n}={v:#x}" for n, v in zip(SWITCH_FRAME, saved))
                print(f"  kernel rsp={rsp + 8 * len(SWITCH_FRAME):#x} {regs}")
            for field in ["exception_frame", "syscall_frame"]:
                frame = scalar(t[field])
                if int(frame) != 0:
                    f = frame.dereference()
                    names = ["rip", "rsp", "rax", "rdi", "rsi", "rdx", "rflags"]
                    regs = " ".join(f"{n}={int(f[n]):#x}" for n in names)
                    print(f"  user ({field.split('_')[0]}) {regs}")


class Handles(gdb.Command):
    """Show the handle table of a process: mikros handles PROCESS"""

    def __init__(self):
        super().__init__("mikros handles", gdb.COMMAND_USER)

    @needs_kernel
    def invoke(self, arg, from_tty):
        table = locked(process(arg)["handles"])
        entry_type = gdb.lookup_type("kernel_core::object::handles::Entry")
        offsets = {f.name: f.bitpos // 8 for f in entry_type.fields()}
        print(f"{'HANDLE':>10}  {'RIGHTS':>6}  {'KOID':>6}  TYPE")
        for index, slot in enumerate(vec_items(table["slots"])):
            entry = int(slot["entry"].address)
            # An empty slot's `Option<Entry>` holds a null object pointer.
            if word(entry + offsets["object"]) == 0:
                continue
            value = (index + 1) << 8 | int(slot["generation"])
            rights = word(entry + offsets["rights"]) & 0xFFFF_FFFF
            kind, koid = object_of(entry + offsets["object"])
            print(f"{value:>#10x}  {rights:>#6x}  {koid:>6}  {kind}")


class Mappings(gdb.Command):
    """Show the mappings of a process: mikros mappings PROCESS"""

    def __init__(self):
        super().__init__("mikros mappings", gdb.COMMAND_USER)

    @needs_kernel
    def invoke(self, arg, from_tty):
        space = arc_inner(process(arg)["aspace"])["data"]
        regions = locked(space["regions"])
        print(f"{'START':>18}  {'END':>18}  PERM  MAX  {'OBJECT':>6}  {'OFFSET':>10}")
        for _, r in rust.children_of_btree_map(regions["map"]):
            start, length = int(r["start"]), int(r["len"])
            mem = int(arc_inner(r["mem"])["data"]["header"]["koid"])
            note = "  (being removed)" if int(r["removal"]) else ""
            print(
                f"{start:>#18x}  {start + length:>#18x}  {perms(int(r['perms'])):<4}  "
                f"{perms(int(r['max'])):<3}  {mem:>6}  {int(r['offset']):>#10x}{note}"
            )


class Log(gdb.Command):
    """Print the newest records of the kernel log ring: mikros log [COUNT]"""

    def __init__(self):
        super().__init__("mikros log", gdb.COMMAND_USER)

    @needs_kernel
    def invoke(self, arg, from_tty):
        log = static("kernel::log::LOG")
        slots = log["slots"]
        count = slots.type.strip_typedefs().range()[1] + 1
        wanted = count_argument(arg) if arg.strip() else count
        head = int(scalar(log["head"]))
        inferior = gdb.selected_inferior()
        for sequence in range(max(1, head - min(wanted, count) + 1), head + 1):
            slot = slots[(sequence - 1) % count]
            # A slot holds a whole record when its state is twice the
            # record's sequence.
            if int(scalar(slot["state"])) != 2 * sequence:
                print(f"[record {sequence} not available]")
                continue
            words = slot["words"]
            raw = bytes(inferior.read_memory(int(words.address), words.type.sizeof))
            timestamp = int.from_bytes(raw[8:16], "little", signed=True)
            koid = int.from_bytes(raw[16:24], "little")
            severity, length = raw[24], int.from_bytes(raw[26:28], "little")
            message = raw[32 : 32 + length].decode(errors="replace")
            seconds, rest = divmod(timestamp, 1_000_000_000)
            level = SEVERITIES[severity] if severity < len(SEVERITIES) else "?"
            print(f"[{seconds}.{rest // 1000:06}] {level} p{koid}: {message}")


Mikros()
Ps()
Threads()
Handles()
Mappings()
Log()
