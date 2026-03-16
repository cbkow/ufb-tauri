param(
    [string]$Title = "New Shot",
    [string]$SourceDir,
    [string]$TargetDir,
    [string]$TemplateFile = ""
)

$appRoot = Split-Path (Split-Path $PSScriptRoot)

Add-Type -AssemblyName PresentationFramework, PresentationCore, WindowsBase

# DWM interop for dark title bar, AppUserModelID for taskbar icon
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class DwmHelper {
    [DllImport("dwmapi.dll", PreserveSig = true)]
    public static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int attrValue, int attrSize);
    [DllImport("shell32.dll", SetLastError = true)]
    public static extern int SetCurrentProcessExplicitAppUserModelID([MarshalAs(UnmanagedType.LPWStr)] string AppID);
}
"@
[DwmHelper]::SetCurrentProcessExplicitAppUserModelID("UFB.UnionProjects") | Out-Null

# Detect system dark mode
$isDark = (Get-ItemProperty -Path "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize" -ErrorAction SilentlyContinue).AppsUseLightTheme -eq 0

# Get system accent color (stored as ABGR)
$accentDword = (Get-ItemProperty -Path "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Accent" -Name "AccentColorMenu" -ErrorAction SilentlyContinue).AccentColorMenu
if ($accentDword) {
    $aR = $accentDword -band 0xFF
    $aG = ($accentDword -shr 8) -band 0xFF
    $aB = ($accentDword -shr 16) -band 0xFF
    $accent = "#{0:X2}{1:X2}{2:X2}" -f $aR, $aG, $aB
    $accentHover = "#{0:X2}{1:X2}{2:X2}" -f ([Math]::Min(255,$aR+30)), ([Math]::Min(255,$aG+30)), ([Math]::Min(255,$aB+30))
    $accentPressed = "#{0:X2}{1:X2}{2:X2}" -f ([Math]::Max(0,$aR-20)), ([Math]::Max(0,$aG-20)), ([Math]::Max(0,$aB-20))
} else {
    $accent = "#0078D4"; $accentHover = "#1A8CE6"; $accentPressed = "#006CBE"
}

if ($isDark) {
    $bg = "#202020"; $fg = "#FFFFFF"; $inputBg = "#383838"; $border = "#555555"
    $cancelBg = "#333333"; $cancelHover = "#444444"; $cancelPressed = "#2A2A2A"
    $cancelFg = "#FFFFFF"
} else {
    $bg = "#F3F3F3"; $fg = "#000000"; $inputBg = "#FFFFFF"; $border = "#CCCCCC"
    $cancelBg = "#E5E5E5"; $cancelHover = "#D5D5D5"; $cancelPressed = "#C5C5C5"
    $cancelFg = "#000000"
}

[xml]$xaml = @"
<Window xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        Title="$Title" Width="420" Height="195"
        WindowStartupLocation="CenterScreen" ResizeMode="NoResize"
        Background="$bg"
        Icon="$appRoot\icons\icon.ico">
    <Window.Resources>
        <Style x:Key="PrimaryButton" TargetType="Button">
            <Setter Property="FocusVisualStyle" Value="{x:Null}"/>
            <Setter Property="FontFamily" Value="Segoe UI"/>
            <Setter Property="FontSize" Value="13"/>
            <Setter Property="Foreground" Value="#FFFFFF"/>
            <Setter Property="Template">
                <Setter.Value>
                    <ControlTemplate TargetType="Button">
                        <Border x:Name="Bd" Background="$accent"
                                CornerRadius="4" Padding="0">
                            <ContentPresenter HorizontalAlignment="Center"
                                              VerticalAlignment="Center"/>
                        </Border>
                        <ControlTemplate.Triggers>
                            <Trigger Property="IsMouseOver" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$accentHover"/>
                            </Trigger>
                            <Trigger Property="IsPressed" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$accentPressed"/>
                            </Trigger>
                        </ControlTemplate.Triggers>
                    </ControlTemplate>
                </Setter.Value>
            </Setter>
        </Style>
        <Style x:Key="AccentTextBox" TargetType="TextBox">
            <Setter Property="FocusVisualStyle" Value="{x:Null}"/>
            <Setter Property="FontFamily" Value="Segoe UI"/>
            <Setter Property="FontSize" Value="14"/>
            <Setter Property="Foreground" Value="$fg"/>
            <Setter Property="CaretBrush" Value="$fg"/>
            <Setter Property="Template">
                <Setter.Value>
                    <ControlTemplate TargetType="TextBox">
                        <Border x:Name="Bd" Background="$inputBg"
                                BorderBrush="$inputBg" BorderThickness="0"
                                CornerRadius="2" Padding="6,4">
                            <ScrollViewer x:Name="PART_ContentHost"/>
                        </Border>
                    </ControlTemplate>
                </Setter.Value>
            </Setter>
        </Style>
        <Style x:Key="SecondaryButton" TargetType="Button">
            <Setter Property="FocusVisualStyle" Value="{x:Null}"/>
            <Setter Property="FontFamily" Value="Segoe UI"/>
            <Setter Property="FontSize" Value="13"/>
            <Setter Property="Foreground" Value="$cancelFg"/>
            <Setter Property="Template">
                <Setter.Value>
                    <ControlTemplate TargetType="Button">
                        <Border x:Name="Bd" Background="$cancelBg"
                                CornerRadius="4" Padding="0">
                            <ContentPresenter HorizontalAlignment="Center"
                                              VerticalAlignment="Center"/>
                        </Border>
                        <ControlTemplate.Triggers>
                            <Trigger Property="IsMouseOver" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$cancelHover"/>
                            </Trigger>
                            <Trigger Property="IsPressed" Value="True">
                                <Setter TargetName="Bd" Property="Background" Value="$cancelPressed"/>
                            </Trigger>
                        </ControlTemplate.Triggers>
                    </ControlTemplate>
                </Setter.Value>
            </Setter>
        </Style>
    </Window.Resources>
    <Grid Margin="20">
        <Grid.RowDefinitions>
            <RowDefinition Height="Auto"/>
            <RowDefinition Height="Auto"/>
            <RowDefinition Height="*"/>
            <RowDefinition Height="Auto"/>
        </Grid.RowDefinitions>
        <TextBlock Grid.Row="0" Text="Enter shot name:" Foreground="$fg"
                   FontFamily="Segoe UI" FontSize="14" Margin="0,0,0,10"/>
        <TextBox Grid.Row="1" x:Name="ShotName"
                 Style="{StaticResource AccentTextBox}"/>
        <StackPanel Grid.Row="3" Orientation="Horizontal" HorizontalAlignment="Right"
                    Margin="0,15,0,0">
            <Button x:Name="OkButton" Content="OK" Width="80" Height="26"
                    Margin="0,0,8,0" Style="{StaticResource PrimaryButton}"
                    IsDefault="True"/>
            <Button x:Name="CancelButton" Content="Cancel" Width="80" Height="26"
                    Style="{StaticResource SecondaryButton}" IsCancel="True"/>
        </StackPanel>
    </Grid>
</Window>
"@

$reader = New-Object System.Xml.XmlNodeReader $xaml
$window = [Windows.Markup.XamlReader]::Load($reader)

# Apply dark title bar via DWM if dark mode
if ($isDark) {
    $window.Add_SourceInitialized({
        $hwnd = (New-Object System.Windows.Interop.WindowInteropHelper $window).Handle
        $value = 1
        [DwmHelper]::DwmSetWindowAttribute($hwnd, 20, [ref]$value, 4)
    })
}

$nameBox = $window.FindName("ShotName")
$okBtn = $window.FindName("OkButton")
$cancelBtn = $window.FindName("CancelButton")

$okBtn.Add_Click({ $window.DialogResult = $true; $window.Close() })
$cancelBtn.Add_Click({ $window.DialogResult = $false; $window.Close() })

$nameBox.Focus() | Out-Null

if ($window.ShowDialog() -and $nameBox.Text.Trim()) {
    $shotName = $nameBox.Text.Trim()
    $destPath = Join-Path $TargetDir $shotName

    # Copy template folder as shot name
    Copy-Item -Path $SourceDir -Destination $destPath -Recurse -Force

    # Rename template file to {shotName}_v001.{ext}
    $templatePath = Join-Path $destPath $TemplateFile
    if (Test-Path $templatePath) {
        $ext = [System.IO.Path]::GetExtension($TemplateFile)
        Rename-Item -Path $templatePath -NewName "${shotName}_v001${ext}"
    }
}
