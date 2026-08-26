Option Explicit
Dim fso, shell, hostFolder, command
Set fso = CreateObject("Scripting.FileSystemObject")
Set shell = CreateObject("WScript.Shell")
hostFolder = fso.GetParentFolderName(WScript.ScriptFullName)
command = Chr(34) & hostFolder & "\run-display-windows.cmd" & Chr(34)
shell.Run command, 0, False
