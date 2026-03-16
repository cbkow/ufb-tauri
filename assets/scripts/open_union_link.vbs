Function URLDecode(str)
    Dim i, ch
    URLDecode = ""
    i = 1
    Do While i <= Len(str)
        ch = Mid(str, i, 1)
        If ch = "%" And i + 2 <= Len(str) Then
            URLDecode = URLDecode & Chr(CInt("&H" & Mid(str, i + 1, 2)))
            i = i + 3
        Else
            URLDecode = URLDecode & ch
            i = i + 1
        End If
    Loop
End Function

Dim uri, path
uri = WScript.Arguments(0)

' Strip union:/// prefix
path = Mid(uri, 10)

' Decode URL encoding and convert slashes
path = URLDecode(path)
path = Replace(path, "/", "\")

' Show in Explorer
Dim shell
Set shell = CreateObject("WScript.Shell")
shell.Run "explorer.exe /select,""" & path & """", 1, False
