menu(title='Union Projects' type='back|directory|drive|desktop' image=\uE0CF)
{
    item(title='New Project' image=\uE0D0
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_project.ps1" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '"'
        window=cmd.hidden)

    item(title='New AE Shot' image=\uE172
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_shot.ps1" -Title "New AE Shot" -SourceDir "{{INSTDIR}}\assets\projectTemplate\ae\_t_project_name" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '" -TemplateFile "project\_template.aep"'
        window=cmd.hidden)

    item(title='New Premiere Shot' image=\uE172
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_shot.ps1" -Title "New Premiere Shot" -SourceDir "{{INSTDIR}}\assets\projectTemplate\premiere\_t_project_name" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '" -TemplateFile "project\_template.prproj"'
        window=cmd.hidden)

    item(title='New Blender Shot' image=\uE172
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_shot.ps1" -Title "New Blender Shot" -SourceDir "{{INSTDIR}}\assets\projectTemplate\3d\_t_project_name" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '" -TemplateFile "project\template.blend"'
        window=cmd.hidden)

    item(title='New Photoshop' image=\uE170
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_shot.ps1" -Title "New Photoshop" -SourceDir "{{INSTDIR}}\assets\projectTemplate\photoshop\_t_project_name" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '"'
        window=cmd.hidden)

    item(title='New Illustrator' image=\uE170
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\new_shot.ps1" -Title "New Illustrator" -SourceDir "{{INSTDIR}}\assets\projectTemplate\illustrator\_t_project_name" -TargetDir "' + if(sel.back, sel.curdir, sel.path) + '"'
        window=cmd.hidden)

    separator(where=str.contains(if(sel.back, sel.curdir, sel.path), 'C:\Volumes\ufb\'))

    item(title='Project Notes' image=\uE10E
        where=str.contains(if(sel.back, sel.curdir, sel.path), 'C:\Volumes\ufb\')
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\project_notes.ps1" -Path "' + if(sel.back, sel.curdir, sel.path) + '" -Mode "doc"'
        window=cmd.hidden)

    item(title='Project Notes Folder' image=\uE10F
        where=str.contains(if(sel.back, sel.curdir, sel.path), 'C:\Volumes\ufb\')
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\project_notes.ps1" -Path "' + if(sel.back, sel.curdir, sel.path) + '" -Mode "folder"'
        window=cmd.hidden)
}
