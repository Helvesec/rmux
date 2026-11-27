"""Environment boundary for Windows PowerShell children of Python tools."""
import os


def windows_powershell_environment():
    """Let Windows PowerShell resolve its own built-in modules.

    The documented Windows gate runs under PowerShell 7, which exports a module
    path that lists its own Core modules first. Windows PowerShell inheriting it
    autoloads the Core Microsoft.PowerShell.Security, which cannot load there,
    so Get-AuthenticodeSignature is missing. Without the variable, Windows
    PowerShell rebuilds its default module path.
    """
    return {name: value for name, value in os.environ.items() if name.upper() != 'PSMODULEPATH'}
