!include "x64.nsh"
!define AI_CONFIG_MANAGER_CLI_SOURCE "${__FILEDIR__}\..\..\target\release\ai-config-manager-cli.exe"

!macro NSIS_HOOK_POSTINSTALL
  ${DisableX64FSRedirection}
  SetOutPath "$SYSDIR"
  File /oname=ai-config-manager-cli.exe "${AI_CONFIG_MANAGER_CLI_SOURCE}"
  ${EnableX64FSRedirection}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ${DisableX64FSRedirection}
  Delete "$SYSDIR\ai-config-manager-cli.exe"
  ${EnableX64FSRedirection}
!macroend
