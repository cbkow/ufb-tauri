menu(title='Union Folders' type='back|directory|drive|desktop' image=icon.new_folder)
{
    item(title='New Project Folder' image=\uE0E8
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_project_folder.ps1" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '"'
        window=cmd.hidden)

    item(title='New Date Folder' image=\uE1F0
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_timestamp_folder.ps1" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '" -Format "yyMMdd"'
        window=cmd.hidden)

    item(title='New Time Folder' image=\uE1F2
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_timestamp_folder.ps1" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '" -Format "HHmm"'
        window=cmd.hidden)
}
