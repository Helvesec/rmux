"""R2-223 approved AST specimens, parsed as data and never executed."""


def relative_or_string(path: Path | None) -> str | None:
    if path is None:
        return None
    resolved = path.resolve()
    # A temporary root inside the checkout still takes privacy precedence.
    public_path = sanitize(str(resolved))
    if public_path != str(resolved):
        return public_path
    try:
        return str(resolved.relative_to(ROOT))
    except ValueError:
        return public_path


def screen_cleanup(executable: str, name: str) -> None:
    deadline = time.monotonic() + 5.0
    directory = clean_env().get("SCREENDIR")
    sockets = []
    if directory is not None:
        try:
            sockets = [path for path in Path(directory).iterdir()
                       if path.name.partition(".")[0].isdigit()
                       and path.name.partition(".")[2] == name]
        except FileNotFoundError:
            pass
    processes = {int(path.name.partition(".")[0]): _screen_process_start(
        int(path.name.partition(".")[0])) for path in sockets}
    quiet(screen_control_command(executable, name, ["-X", "quit"]),
          check=False, timeout=max(0.0, deadline - time.monotonic()))
    # The quit client only sends a request. Observe the server's own unlink
    # and exit, outside the measured sample and within the original budget.
    while True:
        sockets = [path for path in sockets if path.exists()]
        processes = {pid: start for pid, start in processes.items()
                     if start is not None and _screen_process_start(pid) == start}
        if not sockets and not processes:
            return
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise FixtureCleanupError({"tool": "screen", "session": name,
                                       "remaining_sockets": [path.name for path in sockets],
                                       "live_processes": list(processes)})
        time.sleep(min(0.01, remaining))
