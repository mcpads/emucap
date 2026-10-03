# Isolate Windows loader behavior before the Rust qualification suite.
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class EmucapLoaderProbe {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool FreeLibrary(IntPtr module);
}
'@
$library = Join-Path $env:SystemRoot 'System32/kernel32.dll'
foreach ($path in @($library, ('\\?\' + $library))) {
    foreach ($flags in @(0, 0x1100)) {
        $module = [EmucapLoaderProbe]::LoadLibraryExW($path, [IntPtr]::Zero, $flags)
        $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        [pscustomobject]@{ path=$path; flags=$flags; loaded=($module -ne [IntPtr]::Zero); error=$errorCode } | ConvertTo-Json -Compress
        if ($module -ne [IntPtr]::Zero) { [void][EmucapLoaderProbe]::FreeLibrary($module) }
    }
}
