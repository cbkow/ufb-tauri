menu(mode="multiple" title='Union Files' image=\uE114)
{
    item(mode="single" title='Open in u.f.b.' image=image.res('{{INSTDIR}}\{{EXENAME}}', 0)
        cmd='{{INSTDIR}}\{{EXENAME}}' args=sel.path.quote)

    item(mode="single" title='Copy Union Link' image=icon.copy_path
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\copy_union_link.ps1" -Path "' + sel.path + '"'
        window=cmd.hidden)

    item(mode="single" title='Copy ufb Link' image=icon.copy_path
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\copy_ufb_link.ps1" -Path "' + sel.path + '"'
        window=cmd.hidden)

    separator
    item(mode="multiple" type='file' find='.mov' title='Transcode' image=\uE173
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\transcode.ps1" -Paths "' + sel(false, '|') + '"'
        window=cmd.hidden)

    item(mode="single" type='file' find='.mov|.mp4' title='Find AE Project' image=\uE11A
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\ae_finder.ps1" -Path "' + sel.path + '"'
        window=cmd.hidden)

    item(mode="single" type='file' find='.mov|.mp4' title='Find Premiere Project' image=\uE11A
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\premiere_finder.ps1" -Path "' + sel.path + '"'
        window=cmd.hidden)

    separator
    item(mode="single" type='file' find='.aep' title='AE Render' image=\uE102
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\ae_render.ps1" -Path "' + sel.path + '"'
        window=cmd.hidden)

    item(mode="multiple" type='file|dir' title='Move to Z_OLD' image=\uE107
        cmd='powershell.exe'
        args='-ExecutionPolicy Bypass -WindowStyle Hidden -File "{{INSTDIR}}\assets\scripts\move_to_zold.ps1" -Paths "' + sel(false, '|') + '"'
        window=cmd.hidden)

    menu(mode="single" separator="after" title=title.copy_path image=icon.copy_path)
    {
        item(where=sel.count > 1 title='Copy (@sel.count) items selected' cmd=command.copy(sel(false, "\n")))
        item(mode="single" title=@sel.path tip=sel.path cmd=command.copy(sel.path))
        item(mode="single" type='file' separator="before" find='.lnk' title='open file location')
        separator
        item(mode="single" where=@sel.parent.len>3 title=sel.parent cmd=@command.copy(sel.parent))
        separator
        item(mode="single" type='file|dir|back.dir' title=sel.file.name cmd=command.copy(sel.file.name))
        item(mode="single" type='file' where=sel.file.len != sel.file.title.len title=@sel.file.title cmd=command.copy(sel.file.title))
        item(mode="single" type='file' where=sel.file.ext.len>0 title=sel.file.ext cmd=command.copy(sel.file.ext))
    }

}
