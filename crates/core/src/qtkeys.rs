//! WhatPulse Qt 键码 → 显示名映射（PLAN §4.8 契约）。
//!
//! **表来源（唯一来源，禁止凭记忆补值）**：Qt 官方 qtbase `src/corelib/global/qnamespace.h`
//! 的 `enum Key` 全部 482 个显式取值条目，机械提取生成于 2026-09-28：
//! `https://code.qt.io/cgit/qt/qtbase.git/plain/src/corelib/global/qnamespace.h`
//! （Qt 6 dev 分支；生成脚本断言了 §4.8 全部锚定值——含 F1~F24——与官方头文件逐字一致，
//! 同值别名如 Key_Hangul_switch=Key_Mode_switch 取首次出现）。
//!
//! 映射规则（PLAN §4.8 逐字对齐）：
//! 1. `0x20..=0x7E` 可打印 ASCII → 字符本身（字母即大写，如 0x41 → "A"）；
//! 2. `0xA0..=0xFF` Latin-1 可打印区（官方 Key_nobreakspace=0xA0 .. Key_ydiaeresis=0xFF）
//!    → 字符本身（同规则 1 的字符语义，如 0xE9 → "é"；值域来自官方头文件）；
//! 3. 已知特殊码（[QT_SPECIAL_KEYS]，官方头文件 ≥0x01000000 全量条目）→ 静态表标签
//!    （§4.8 锚定条目用 PLAN 标签：Key_Control→"Ctrl"、Key_Meta→"Win"、
//!    Key_Enter→"Enter(小键盘)"、方向键→"←↑→↓"、Key_Back/Key_Forward→"BrowserBack/Forward"）；
//! 4. 其余 → `键 0x{code:X}`。
//!
//! **Qt 没有 Key_NumPad0..9**（小键盘数字即 Key_0..9 = 0x30..0x39 + KeypadModifier，
//! 已对照官方头文件核实：全文件不含 NumPad 字样），本表禁止杜撰该段。
//!
//! 标签语言与 §4.8 锚定条目一致（英文键名 + 必要的中文消歧），供 `wp_key_daily.label` /
//! `wp_combo_daily.label` 直接落库；GUI 侧布局相关的键帽显示（GetKeyNameTextW）不经过本表。

/// Qt 特殊键静态表：`(code, label)`，按 code 升序、无重复（同值别名取官方头文件首次出现）。
/// 由 qnamespace.h 机械生成——不要手改行内容；重新生成须走提取脚本。
pub static QT_SPECIAL_KEYS: &[(i64, &str)] = &[
    (0x01000000, "Escape"), // Key_Escape
    (0x01000001, "Tab"), // Key_Tab
    (0x01000002, "Backtab"), // Key_Backtab
    (0x01000003, "Backspace"), // Key_Backspace
    (0x01000004, "Return"), // Key_Return
    (0x01000005, "Enter(小键盘)"), // Key_Enter
    (0x01000006, "Insert"), // Key_Insert
    (0x01000007, "Delete"), // Key_Delete
    (0x01000008, "Pause"), // Key_Pause
    (0x01000009, "Print"), // Key_Print
    (0x0100000A, "SysReq"), // Key_SysReq
    (0x0100000B, "Clear"), // Key_Clear
    (0x01000010, "Home"), // Key_Home
    (0x01000011, "End"), // Key_End
    (0x01000012, "←"), // Key_Left
    (0x01000013, "↑"), // Key_Up
    (0x01000014, "→"), // Key_Right
    (0x01000015, "↓"), // Key_Down
    (0x01000016, "PageUp"), // Key_PageUp
    (0x01000017, "PageDown"), // Key_PageDown
    (0x01000020, "Shift"), // Key_Shift
    (0x01000021, "Ctrl"), // Key_Control
    (0x01000022, "Win"), // Key_Meta
    (0x01000023, "Alt"), // Key_Alt
    (0x01000024, "CapsLock"), // Key_CapsLock
    (0x01000025, "NumLock"), // Key_NumLock
    (0x01000026, "ScrollLock"), // Key_ScrollLock
    (0x01000030, "F1"), // Key_F1
    (0x01000031, "F2"), // Key_F2
    (0x01000032, "F3"), // Key_F3
    (0x01000033, "F4"), // Key_F4
    (0x01000034, "F5"), // Key_F5
    (0x01000035, "F6"), // Key_F6
    (0x01000036, "F7"), // Key_F7
    (0x01000037, "F8"), // Key_F8
    (0x01000038, "F9"), // Key_F9
    (0x01000039, "F10"), // Key_F10
    (0x0100003A, "F11"), // Key_F11
    (0x0100003B, "F12"), // Key_F12
    (0x0100003C, "F13"), // Key_F13
    (0x0100003D, "F14"), // Key_F14
    (0x0100003E, "F15"), // Key_F15
    (0x0100003F, "F16"), // Key_F16
    (0x01000040, "F17"), // Key_F17
    (0x01000041, "F18"), // Key_F18
    (0x01000042, "F19"), // Key_F19
    (0x01000043, "F20"), // Key_F20
    (0x01000044, "F21"), // Key_F21
    (0x01000045, "F22"), // Key_F22
    (0x01000046, "F23"), // Key_F23
    (0x01000047, "F24"), // Key_F24
    (0x01000048, "F25"), // Key_F25
    (0x01000049, "F26"), // Key_F26
    (0x0100004A, "F27"), // Key_F27
    (0x0100004B, "F28"), // Key_F28
    (0x0100004C, "F29"), // Key_F29
    (0x0100004D, "F30"), // Key_F30
    (0x0100004E, "F31"), // Key_F31
    (0x0100004F, "F32"), // Key_F32
    (0x01000050, "F33"), // Key_F33
    (0x01000051, "F34"), // Key_F34
    (0x01000052, "F35"), // Key_F35
    (0x01000053, "Super_L"), // Key_Super_L
    (0x01000054, "Super_R"), // Key_Super_R
    (0x01000055, "Menu"), // Key_Menu
    (0x01000056, "Hyper_L"), // Key_Hyper_L
    (0x01000057, "Hyper_R"), // Key_Hyper_R
    (0x01000058, "Help"), // Key_Help
    (0x01000059, "Direction_L"), // Key_Direction_L
    (0x01000060, "Direction_R"), // Key_Direction_R
    (0x01000061, "BrowserBack"), // Key_Back
    (0x01000062, "BrowserForward"), // Key_Forward
    (0x01000063, "Stop"), // Key_Stop
    (0x01000064, "Refresh"), // Key_Refresh
    (0x01000070, "VolumeDown"), // Key_VolumeDown
    (0x01000071, "VolumeMute"), // Key_VolumeMute
    (0x01000072, "VolumeUp"), // Key_VolumeUp
    (0x01000073, "BassBoost"), // Key_BassBoost
    (0x01000074, "BassUp"), // Key_BassUp
    (0x01000075, "BassDown"), // Key_BassDown
    (0x01000076, "TrebleUp"), // Key_TrebleUp
    (0x01000077, "TrebleDown"), // Key_TrebleDown
    (0x01000080, "MediaPlay"), // Key_MediaPlay
    (0x01000081, "MediaStop"), // Key_MediaStop
    (0x01000082, "MediaPrevious"), // Key_MediaPrevious
    (0x01000083, "MediaNext"), // Key_MediaNext
    (0x01000084, "MediaRecord"), // Key_MediaRecord
    (0x01000085, "MediaPause"), // Key_MediaPause
    (0x01000086, "MediaTogglePlayPause"), // Key_MediaTogglePlayPause
    (0x01000090, "HomePage"), // Key_HomePage
    (0x01000091, "Favorites"), // Key_Favorites
    (0x01000092, "Search"), // Key_Search
    (0x01000093, "Standby"), // Key_Standby
    (0x01000094, "OpenUrl"), // Key_OpenUrl
    (0x010000A0, "LaunchMail"), // Key_LaunchMail
    (0x010000A1, "LaunchMedia"), // Key_LaunchMedia
    (0x010000A2, "Launch0"), // Key_Launch0
    (0x010000A3, "Launch1"), // Key_Launch1
    (0x010000A4, "Launch2"), // Key_Launch2
    (0x010000A5, "Launch3"), // Key_Launch3
    (0x010000A6, "Launch4"), // Key_Launch4
    (0x010000A7, "Launch5"), // Key_Launch5
    (0x010000A8, "Launch6"), // Key_Launch6
    (0x010000A9, "Launch7"), // Key_Launch7
    (0x010000AA, "Launch8"), // Key_Launch8
    (0x010000AB, "Launch9"), // Key_Launch9
    (0x010000AC, "LaunchA"), // Key_LaunchA
    (0x010000AD, "LaunchB"), // Key_LaunchB
    (0x010000AE, "LaunchC"), // Key_LaunchC
    (0x010000AF, "LaunchD"), // Key_LaunchD
    (0x010000B0, "LaunchE"), // Key_LaunchE
    (0x010000B1, "LaunchF"), // Key_LaunchF
    (0x010000B2, "MonBrightnessUp"), // Key_MonBrightnessUp
    (0x010000B3, "MonBrightnessDown"), // Key_MonBrightnessDown
    (0x010000B4, "KeyboardLightOnOff"), // Key_KeyboardLightOnOff
    (0x010000B5, "KeyboardBrightnessUp"), // Key_KeyboardBrightnessUp
    (0x010000B6, "KeyboardBrightnessDown"), // Key_KeyboardBrightnessDown
    (0x010000B7, "PowerOff"), // Key_PowerOff
    (0x010000B8, "WakeUp"), // Key_WakeUp
    (0x010000B9, "Eject"), // Key_Eject
    (0x010000BA, "ScreenSaver"), // Key_ScreenSaver
    (0x010000BB, "WWW"), // Key_WWW
    (0x010000BC, "Memo"), // Key_Memo
    (0x010000BD, "LightBulb"), // Key_LightBulb
    (0x010000BE, "Shop"), // Key_Shop
    (0x010000BF, "History"), // Key_History
    (0x010000C0, "AddFavorite"), // Key_AddFavorite
    (0x010000C1, "HotLinks"), // Key_HotLinks
    (0x010000C2, "BrightnessAdjust"), // Key_BrightnessAdjust
    (0x010000C3, "Finance"), // Key_Finance
    (0x010000C4, "Community"), // Key_Community
    (0x010000C5, "AudioRewind"), // Key_AudioRewind
    (0x010000C6, "BackForward"), // Key_BackForward
    (0x010000C7, "ApplicationLeft"), // Key_ApplicationLeft
    (0x010000C8, "ApplicationRight"), // Key_ApplicationRight
    (0x010000C9, "Book"), // Key_Book
    (0x010000CA, "CD"), // Key_CD
    (0x010000CB, "Calculator"), // Key_Calculator
    (0x010000CC, "ToDoList"), // Key_ToDoList
    (0x010000CD, "ClearGrab"), // Key_ClearGrab
    (0x010000CE, "Close"), // Key_Close
    (0x010000CF, "Copy"), // Key_Copy
    (0x010000D0, "Cut"), // Key_Cut
    (0x010000D1, "Display"), // Key_Display
    (0x010000D2, "DOS"), // Key_DOS
    (0x010000D3, "Documents"), // Key_Documents
    (0x010000D4, "Excel"), // Key_Excel
    (0x010000D5, "Explorer"), // Key_Explorer
    (0x010000D6, "Game"), // Key_Game
    (0x010000D7, "Go"), // Key_Go
    (0x010000D8, "iTouch"), // Key_iTouch
    (0x010000D9, "LogOff"), // Key_LogOff
    (0x010000DA, "Market"), // Key_Market
    (0x010000DB, "Meeting"), // Key_Meeting
    (0x010000DC, "MenuKB"), // Key_MenuKB
    (0x010000DD, "MenuPB"), // Key_MenuPB
    (0x010000DE, "MySites"), // Key_MySites
    (0x010000DF, "News"), // Key_News
    (0x010000E0, "OfficeHome"), // Key_OfficeHome
    (0x010000E1, "Option"), // Key_Option
    (0x010000E2, "Paste"), // Key_Paste
    (0x010000E3, "Phone"), // Key_Phone
    (0x010000E4, "Calendar"), // Key_Calendar
    (0x010000E5, "Reply"), // Key_Reply
    (0x010000E6, "Reload"), // Key_Reload
    (0x010000E7, "RotateWindows"), // Key_RotateWindows
    (0x010000E8, "RotationPB"), // Key_RotationPB
    (0x010000E9, "RotationKB"), // Key_RotationKB
    (0x010000EA, "Save"), // Key_Save
    (0x010000EB, "Send"), // Key_Send
    (0x010000EC, "Spell"), // Key_Spell
    (0x010000ED, "SplitScreen"), // Key_SplitScreen
    (0x010000EE, "Support"), // Key_Support
    (0x010000EF, "TaskPane"), // Key_TaskPane
    (0x010000F0, "Terminal"), // Key_Terminal
    (0x010000F1, "Tools"), // Key_Tools
    (0x010000F2, "Travel"), // Key_Travel
    (0x010000F3, "Video"), // Key_Video
    (0x010000F4, "Word"), // Key_Word
    (0x010000F5, "Xfer"), // Key_Xfer
    (0x010000F6, "ZoomIn"), // Key_ZoomIn
    (0x010000F7, "ZoomOut"), // Key_ZoomOut
    (0x010000F8, "Away"), // Key_Away
    (0x010000F9, "Messenger"), // Key_Messenger
    (0x010000FA, "WebCam"), // Key_WebCam
    (0x010000FB, "MailForward"), // Key_MailForward
    (0x010000FC, "Pictures"), // Key_Pictures
    (0x010000FD, "Music"), // Key_Music
    (0x010000FE, "Battery"), // Key_Battery
    (0x010000FF, "Bluetooth"), // Key_Bluetooth
    (0x01000100, "WLAN"), // Key_WLAN
    (0x01000101, "UWB"), // Key_UWB
    (0x01000102, "AudioForward"), // Key_AudioForward
    (0x01000103, "AudioRepeat"), // Key_AudioRepeat
    (0x01000104, "AudioRandomPlay"), // Key_AudioRandomPlay
    (0x01000105, "Subtitle"), // Key_Subtitle
    (0x01000106, "AudioCycleTrack"), // Key_AudioCycleTrack
    (0x01000107, "Time"), // Key_Time
    (0x01000108, "Hibernate"), // Key_Hibernate
    (0x01000109, "View"), // Key_View
    (0x0100010A, "TopMenu"), // Key_TopMenu
    (0x0100010B, "PowerDown"), // Key_PowerDown
    (0x0100010C, "Suspend"), // Key_Suspend
    (0x0100010D, "ContrastAdjust"), // Key_ContrastAdjust
    (0x0100010E, "LaunchG"), // Key_LaunchG
    (0x0100010F, "LaunchH"), // Key_LaunchH
    (0x01000110, "TouchpadToggle"), // Key_TouchpadToggle
    (0x01000111, "TouchpadOn"), // Key_TouchpadOn
    (0x01000112, "TouchpadOff"), // Key_TouchpadOff
    (0x01000113, "MicMute"), // Key_MicMute
    (0x01000114, "Red"), // Key_Red
    (0x01000115, "Green"), // Key_Green
    (0x01000116, "Yellow"), // Key_Yellow
    (0x01000117, "Blue"), // Key_Blue
    (0x01000118, "ChannelUp"), // Key_ChannelUp
    (0x01000119, "ChannelDown"), // Key_ChannelDown
    (0x0100011A, "Guide"), // Key_Guide
    (0x0100011B, "Info"), // Key_Info
    (0x0100011C, "Settings"), // Key_Settings
    (0x0100011D, "MicVolumeUp"), // Key_MicVolumeUp
    (0x0100011E, "MicVolumeDown"), // Key_MicVolumeDown
    (0x0100011F, "Keyboard"), // Key_Keyboard
    (0x01000120, "New"), // Key_New
    (0x01000121, "Open"), // Key_Open
    (0x01000122, "Find"), // Key_Find
    (0x01000123, "Undo"), // Key_Undo
    (0x01000124, "Redo"), // Key_Redo
    (0x01001103, "AltGr"), // Key_AltGr
    (0x01001120, "Multi_key"), // Key_Multi_key
    (0x01001121, "Kanji"), // Key_Kanji
    (0x01001122, "Muhenkan"), // Key_Muhenkan
    (0x01001123, "Henkan_Mode"), // Key_Henkan_Mode
    (0x01001124, "Romaji"), // Key_Romaji
    (0x01001125, "Hiragana"), // Key_Hiragana
    (0x01001126, "Katakana"), // Key_Katakana
    (0x01001127, "Hiragana_Katakana"), // Key_Hiragana_Katakana
    (0x01001128, "Zenkaku"), // Key_Zenkaku
    (0x01001129, "Hankaku"), // Key_Hankaku
    (0x0100112A, "Zenkaku_Hankaku"), // Key_Zenkaku_Hankaku
    (0x0100112B, "Touroku"), // Key_Touroku
    (0x0100112C, "Massyo"), // Key_Massyo
    (0x0100112D, "Kana_Lock"), // Key_Kana_Lock
    (0x0100112E, "Kana_Shift"), // Key_Kana_Shift
    (0x0100112F, "Eisu_Shift"), // Key_Eisu_Shift
    (0x01001130, "Eisu_toggle"), // Key_Eisu_toggle
    (0x01001131, "Hangul"), // Key_Hangul
    (0x01001132, "Hangul_Start"), // Key_Hangul_Start
    (0x01001133, "Hangul_End"), // Key_Hangul_End
    (0x01001134, "Hangul_Hanja"), // Key_Hangul_Hanja
    (0x01001135, "Hangul_Jamo"), // Key_Hangul_Jamo
    (0x01001136, "Hangul_Romaja"), // Key_Hangul_Romaja
    (0x01001137, "Codeinput"), // Key_Codeinput
    (0x01001138, "Hangul_Jeonja"), // Key_Hangul_Jeonja
    (0x01001139, "Hangul_Banja"), // Key_Hangul_Banja
    (0x0100113A, "Hangul_PreHanja"), // Key_Hangul_PreHanja
    (0x0100113B, "Hangul_PostHanja"), // Key_Hangul_PostHanja
    (0x0100113C, "SingleCandidate"), // Key_SingleCandidate
    (0x0100113D, "MultipleCandidate"), // Key_MultipleCandidate
    (0x0100113E, "PreviousCandidate"), // Key_PreviousCandidate
    (0x0100113F, "Hangul_Special"), // Key_Hangul_Special
    (0x0100117E, "Mode_switch"), // Key_Mode_switch
    (0x01001250, "Dead_Grave"), // Key_Dead_Grave
    (0x01001251, "Dead_Acute"), // Key_Dead_Acute
    (0x01001252, "Dead_Circumflex"), // Key_Dead_Circumflex
    (0x01001253, "Dead_Tilde"), // Key_Dead_Tilde
    (0x01001254, "Dead_Macron"), // Key_Dead_Macron
    (0x01001255, "Dead_Breve"), // Key_Dead_Breve
    (0x01001256, "Dead_Abovedot"), // Key_Dead_Abovedot
    (0x01001257, "Dead_Diaeresis"), // Key_Dead_Diaeresis
    (0x01001258, "Dead_Abovering"), // Key_Dead_Abovering
    (0x01001259, "Dead_Doubleacute"), // Key_Dead_Doubleacute
    (0x0100125A, "Dead_Caron"), // Key_Dead_Caron
    (0x0100125B, "Dead_Cedilla"), // Key_Dead_Cedilla
    (0x0100125C, "Dead_Ogonek"), // Key_Dead_Ogonek
    (0x0100125D, "Dead_Iota"), // Key_Dead_Iota
    (0x0100125E, "Dead_Voiced_Sound"), // Key_Dead_Voiced_Sound
    (0x0100125F, "Dead_Semivoiced_Sound"), // Key_Dead_Semivoiced_Sound
    (0x01001260, "Dead_Belowdot"), // Key_Dead_Belowdot
    (0x01001261, "Dead_Hook"), // Key_Dead_Hook
    (0x01001262, "Dead_Horn"), // Key_Dead_Horn
    (0x01001263, "Dead_Stroke"), // Key_Dead_Stroke
    (0x01001264, "Dead_Abovecomma"), // Key_Dead_Abovecomma
    (0x01001265, "Dead_Abovereversedcomma"), // Key_Dead_Abovereversedcomma
    (0x01001266, "Dead_Doublegrave"), // Key_Dead_Doublegrave
    (0x01001267, "Dead_Belowring"), // Key_Dead_Belowring
    (0x01001268, "Dead_Belowmacron"), // Key_Dead_Belowmacron
    (0x01001269, "Dead_Belowcircumflex"), // Key_Dead_Belowcircumflex
    (0x0100126A, "Dead_Belowtilde"), // Key_Dead_Belowtilde
    (0x0100126B, "Dead_Belowbreve"), // Key_Dead_Belowbreve
    (0x0100126C, "Dead_Belowdiaeresis"), // Key_Dead_Belowdiaeresis
    (0x0100126D, "Dead_Invertedbreve"), // Key_Dead_Invertedbreve
    (0x0100126E, "Dead_Belowcomma"), // Key_Dead_Belowcomma
    (0x0100126F, "Dead_Currency"), // Key_Dead_Currency
    (0x01001280, "Dead_a"), // Key_Dead_a
    (0x01001281, "Dead_A"), // Key_Dead_A
    (0x01001282, "Dead_e"), // Key_Dead_e
    (0x01001283, "Dead_E"), // Key_Dead_E
    (0x01001284, "Dead_i"), // Key_Dead_i
    (0x01001285, "Dead_I"), // Key_Dead_I
    (0x01001286, "Dead_o"), // Key_Dead_o
    (0x01001287, "Dead_O"), // Key_Dead_O
    (0x01001288, "Dead_u"), // Key_Dead_u
    (0x01001289, "Dead_U"), // Key_Dead_U
    (0x0100128A, "Dead_Small_Schwa"), // Key_Dead_Small_Schwa
    (0x0100128B, "Dead_Capital_Schwa"), // Key_Dead_Capital_Schwa
    (0x0100128C, "Dead_Greek"), // Key_Dead_Greek
    (0x01001290, "Dead_Lowline"), // Key_Dead_Lowline
    (0x01001291, "Dead_Aboveverticalline"), // Key_Dead_Aboveverticalline
    (0x01001292, "Dead_Belowverticalline"), // Key_Dead_Belowverticalline
    (0x01001293, "Dead_Longsolidusoverlay"), // Key_Dead_Longsolidusoverlay
    (0x0100FFFF, "MediaLast"), // Key_MediaLast
    (0x01010000, "Select"), // Key_Select
    (0x01010001, "Yes"), // Key_Yes
    (0x01010002, "No"), // Key_No
    (0x01020001, "Cancel"), // Key_Cancel
    (0x01020002, "Printer"), // Key_Printer
    (0x01020003, "Execute"), // Key_Execute
    (0x01020004, "Sleep"), // Key_Sleep
    (0x01020005, "Play"), // Key_Play
    (0x01020006, "Zoom"), // Key_Zoom
    (0x01020007, "Jisho"), // Key_Jisho
    (0x01020008, "Oyayubi_Left"), // Key_Oyayubi_Left
    (0x01020009, "Oyayubi_Right"), // Key_Oyayubi_Right
    (0x0102000A, "Exit"), // Key_Exit
    (0x01100000, "Context1"), // Key_Context1
    (0x01100001, "Context2"), // Key_Context2
    (0x01100002, "Context3"), // Key_Context3
    (0x01100003, "Context4"), // Key_Context4
    (0x01100004, "Call"), // Key_Call
    (0x01100005, "Hangup"), // Key_Hangup
    (0x01100006, "Flip"), // Key_Flip
    (0x01100007, "ToggleCallHangup"), // Key_ToggleCallHangup
    (0x01100008, "VoiceDial"), // Key_VoiceDial
    (0x01100009, "LastNumberRedial"), // Key_LastNumberRedial
    (0x01100020, "Camera"), // Key_Camera
    (0x01100021, "CameraFocus"), // Key_CameraFocus
    (0x01FFFFFF, "unknown"), // Key_unknown
];

/// WhatPulse Qt 键码 → 显示名（PLAN §4.8 `qt_key_label` 契约）。
///
/// 规则顺序：ASCII 可打印区 → Latin-1 可打印区 → 特殊键静态表 → `键 0x{code:X}` 兜底。
/// 负数/未知码走兜底，绝不 panic。
#[must_use]
pub fn qt_key_label(code: i64) -> String {
    // 1) 0x20~0x7E 可打印 ASCII → 字符本身（大写字母）
    if (0x20..=0x7E).contains(&code) {
        return char::from_u32(code as u32).unwrap_or('\u{FFFD}').to_string();
    }
    // 2) 0xA0~0xFF Latin-1 可打印区（官方头文件 Key_nobreakspace..Key_ydiaeresis 值域）→ 字符本身
    if (0xA0..=0xFF).contains(&code) {
        return char::from_u32(code as u32).unwrap_or('\u{FFFD}').to_string();
    }
    // 3) 已知特殊码静态表（官方 qnamespace.h 生成全表）
    if let Ok(idx) = QT_SPECIAL_KEYS.binary_search_by_key(&code, |e| e.0) {
        return QT_SPECIAL_KEYS[idx].1.to_string();
    }
    // 4) 兜底：键 0x{code:X}
    format!("键 0x{code:X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PLAN §4.8 锚定条目（码值 + 标签）——S2 验收点："qtkeys 表含 §4.8 全部条目"。
    const ANCHORED: &[(i64, &str)] = &[
        (0x01000000, "Escape"), (0x01000001, "Tab"), (0x01000002, "Backtab"),
        (0x01000003, "Backspace"), (0x01000004, "Return"), (0x01000005, "Enter(小键盘)"),
        (0x01000006, "Insert"), (0x01000007, "Delete"), (0x01000008, "Pause"),
        (0x01000009, "Print"), (0x0100000A, "SysReq"), (0x0100000B, "Clear"),
        (0x01000010, "Home"), (0x01000011, "End"), (0x01000012, "←"), (0x01000013, "↑"),
        (0x01000014, "→"), (0x01000015, "↓"), (0x01000016, "PageUp"), (0x01000017, "PageDown"),
        (0x01000020, "Shift"), (0x01000021, "Ctrl"), (0x01000022, "Win"), (0x01000023, "Alt"),
        (0x01000024, "CapsLock"), (0x01000025, "NumLock"), (0x01000026, "ScrollLock"),
        (0x01000030, "F1"), (0x01000031, "F2"), (0x01000035, "F6"), (0x0100003F, "F16"),
        (0x01000041, "F18"), (0x01000047, "F24"),
        (0x01000061, "BrowserBack"), (0x01000062, "BrowserForward"),
        (0x01000070, "VolumeDown"), (0x01000071, "VolumeMute"), (0x01000072, "VolumeUp"),
        (0x01000080, "MediaPlay"), (0x01000081, "MediaStop"), (0x01000082, "MediaPrevious"),
        (0x01000083, "MediaNext"), (0x01000086, "MediaTogglePlayPause"),
        (0x01001103, "AltGr"),
    ];

    #[test]
    fn anchored_entries_all_present() {
        for &(code, label) in ANCHORED {
            assert_eq!(qt_key_label(code), label, "0x{code:08X}");
        }
    }

    #[test]
    fn table_contains_all_anchored_codes() {
        for &(code, _) in ANCHORED {
            assert!(
                QT_SPECIAL_KEYS.iter().any(|&(c, _)| c == code),
                "静态表缺少锚定码 0x{code:08X}"
            );
        }
    }

    #[test]
    fn table_is_sorted_and_unique() {
        for w in QT_SPECIAL_KEYS.windows(2) {
            assert!(w[0].0 < w[1].0, "表必须升序且无重复: 0x{:08X} vs 0x{:08X}", w[0].0, w[1].0);
        }
    }

    #[test]
    fn table_labels_non_empty() {
        assert!(!QT_SPECIAL_KEYS.is_empty());
        for &(_, label) in QT_SPECIAL_KEYS {
            assert!(!label.is_empty(), "标签不得为空");
        }
    }

    #[test]
    fn printable_ascii_is_itself() {
        assert_eq!(qt_key_label(0x20), " "); // Key_Space（契约：字符本身）
        assert_eq!(qt_key_label(0x21), "!");
        assert_eq!(qt_key_label(0x30), "0");
        assert_eq!(qt_key_label(0x39), "9");
        assert_eq!(qt_key_label(0x41), "A"); // Key_A → "A"（大写字母）
        assert_eq!(qt_key_label(0x5A), "Z");
        assert_eq!(qt_key_label(0x61), "a");
        assert_eq!(qt_key_label(0x7E), "~");
        // 全区段逐字符校验
        for c in 0x20u32..=0x7E {
            assert_eq!(qt_key_label(c as i64), char::from_u32(c).unwrap().to_string());
        }
    }

    #[test]
    fn latin1_printable_is_itself() {
        assert_eq!(qt_key_label(0xE1), "á"); // Key_aacute
        assert_eq!(qt_key_label(0xE9), "é"); // Key_eacute
        assert_eq!(qt_key_label(0xFF), "ÿ"); // Key_ydiaeresis（官方头文件 0x0ff）
    }

    #[test]
    fn numpad_segment_must_not_be_fabricated() {
        // Qt 没有 Key_NumPad0..9（小键盘数字 = Key_0..9 + KeypadModifier，§4.8 禁止杜撰）：
        // 断言表中不存在任何 NumPad 标签，特殊键区不被杜撰出小键盘数字段。
        assert!(QT_SPECIAL_KEYS.iter().all(|&(_, l)| !l.starts_with("NumPad")));
        for code in [0x01000050i64, 0x01000059, 0x0100005A] {
            let label = qt_key_label(code);
            assert!(!label.chars().all(|c| c.is_ascii_digit()), "0x{code:08X} 不应杜撰为数字键: {label}");
        }
    }

    #[test]
    fn fallback_format() {
        assert_eq!(qt_key_label(0x12345), "键 0x12345");
        assert_eq!(qt_key_label(0x7F), "键 0x7F"); // DEL 非可打印、非 Qt 键
        assert_eq!(qt_key_label(0x100), "键 0x100"); // Latin-1 与特殊键区之间
        assert_eq!(qt_key_label(0), "键 0x0");
        assert_eq!(qt_key_label(-1), "键 0xFFFFFFFFFFFFFFFF"); // 负数兜底不 panic
    }
}
