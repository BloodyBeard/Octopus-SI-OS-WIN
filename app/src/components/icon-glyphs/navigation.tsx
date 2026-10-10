import { registerGlyphPack, type GlyphDef, type GlyphName } from "../IconGlyphs";

const stroke = { fill: "none", stroke: "currentColor", strokeWidth: 1.33, strokeLinecap: "round", strokeLinejoin: "round" } as const;

const glyphs = {
  archive: {
    viewBox: "0 0 20 20",
    node: <><rect {...stroke} x="2.8" y="3.2" width="14.4" height="3.4" rx="1" /><path {...stroke} d="M4 6.6v8.5a1.7 1.7 0 0 0 1.7 1.7h8.6a1.7 1.7 0 0 0 1.7-1.7V6.6M8 10.1h4" /></>,
  },
  homeFilled: {
    viewBox: "0 0 20 20",
    node: <path fill="currentColor" d="M9.16359 2.63179C9.7105 2.48208 10.2885 2.48214 10.8355 2.63179L11.0855 2.7148C11.212 2.76439 11.3333 2.82555 11.4546 2.89351C11.5671 2.95641 11.6803 3.02484 11.7935 3.10152C12.1006 3.30956 12.4552 3.5894 12.8902 3.93257L17.0786 7.23823L17.3316 7.43843V7.44038L18.3277 8.22749C18.6159 8.45506 18.6655 8.87284 18.438 9.16109C18.2104 9.44919 17.7916 9.49799 17.5034 9.27046L17.3316 9.13472V14C17.3316 14.4554 17.3325 14.8372 17.3071 15.1484C17.2811 15.4674 17.2242 15.771 17.0777 16.0585C16.8542 16.497 16.4975 16.8535 16.0591 17.0771C15.7716 17.2236 15.4679 17.2805 15.1489 17.3066C14.8378 17.332 14.4559 17.332 14.0005 17.332H12.0435V14.1669C12.0435 13.0387 11.1287 12.1234 10.0005 12.123C8.87201 12.123 7.95656 13.0384 7.95656 14.1669V17.332H6.00051C5.54496 17.332 5.1623 17.332 4.85109 17.3066C4.53232 17.2805 4.22932 17.2234 3.94191 17.0771C3.50329 16.8536 3.14594 16.4971 2.92238 16.0585C2.7759 15.771 2.71994 15.4673 2.69387 15.1484C2.66847 14.8372 2.66848 14.4554 2.66848 14V9.13277L2.49465 9.27046C2.20648 9.49779 1.78862 9.44906 1.56105 9.16109C1.33363 8.87297 1.38258 8.45514 1.67043 8.22749L2.66848 7.43941L2.92141 7.23823L7.10891 3.93355C7.54367 3.59031 7.89757 3.31064 8.20461 3.10249C8.23921 3.07904 8.27472 3.05734 8.3091 3.03511C8.38762 2.98434 8.46593 2.93742 8.54445 2.89351C8.74385 2.78197 8.94502 2.69167 9.16359 2.63179Z" />,
  },
  tasks: {
    viewBox: "0 0 20 20",
    node: <><rect {...stroke} x="4.2" y="3" width="11.6" height="14.5" rx="2.3" /><path {...stroke} d="M7.5 2.5h5v2.2h-5zM7.2 8.2h5.6M7.2 11.3h5.6M7.2 14.4h3.1" /></>,
  },
  tasksFilled: {
    viewBox: "0 0 20 20",
    node: <><path fill="currentColor" fillRule="evenodd" d="M6.5 2.8h.3v2.5h6.4V2.8h.3a3 3 0 0 1 3 3v9.6a3 3 0 0 1-3 3h-7a3 3 0 0 1-3-3V5.8a3 3 0 0 1 3-3Zm.7 4.8a.65.65 0 0 0 0 1.3h5.6a.65.65 0 0 0 0-1.3H7.2Zm0 3.1a.65.65 0 0 0 0 1.3h5.6a.65.65 0 0 0 0-1.3H7.2Zm0 3.1a.65.65 0 0 0 0 1.3h3.1a.65.65 0 0 0 0-1.3H7.2Z" /><rect fill="currentColor" x="7.5" y="1.8" width="5" height="2.2" rx=".6" /></>,
  },
  dashboardRail: {
    viewBox: "0 0 24 24",
    node: <g {...stroke} strokeWidth={1.6}><rect x="3" y="3" width="7" height="9" rx="1.5" /><rect x="14" y="3" width="7" height="5" rx="1.5" /><rect x="14" y="12" width="7" height="9" rx="1.5" /><rect x="3" y="16" width="7" height="5" rx="1.5" /></g>,
  },
  dashboardRailFilled: {
    viewBox: "0 0 24 24",
    node: <g fill="currentColor"><rect x="2.2" y="2.2" width="8.6" height="10.6" rx="2.3" /><rect x="13.2" y="2.2" width="8.6" height="6.6" rx="2.3" /><rect x="13.2" y="11.2" width="8.6" height="10.6" rx="2.3" /><rect x="2.2" y="15.2" width="8.6" height="6.6" rx="2.3" /></g>,
  },
  inbox: {
    viewBox: "0 0 20 20",
    node: <><path {...stroke} d="M3.3 10.7 5.1 4.5a1.8 1.8 0 0 1 1.7-1.3h6.4a1.8 1.8 0 0 1 1.7 1.3l1.8 6.2v4.4a1.8 1.8 0 0 1-1.8 1.8H5.1a1.8 1.8 0 0 1-1.8-1.8z" /><path {...stroke} d="M3.4 10.7h4l1 2h3.2l1-2h4" /></>,
  },
  inboxFilled: {
    viewBox: "0 0 20 20",
    node: <path fill="currentColor" d="M6.8 2.5h6.4a2.5 2.5 0 0 1 2.3 1.8l1.6 5.7h-4.9l-1 2H8.8l-1-2H2.9l1.6-5.7a2.5 2.5 0 0 1 2.3-1.8Zm-4.2 8.8H7l1 2h4l1-2h4.4v3.8a2.5 2.5 0 0 1-2.5 2.5H5.1a2.5 2.5 0 0 1-2.5-2.5v-3.8Z" />,
  },
  applicationsRail: {
    viewBox: "0 0 20 20",
    node: <><rect {...stroke} x="3.2" y="3.2" width="5.6" height="5.6" rx="1.6" /><rect {...stroke} x="3.2" y="11.2" width="5.6" height="5.6" rx="1.6" /><rect {...stroke} x="11.2" y="11.2" width="5.6" height="5.6" rx="1.6" /><circle {...stroke} cx="14" cy="6" r="2.9" /></>,
  },
  applicationsRailFilled: {
    viewBox: "0 0 20 20",
    node: <><rect fill="currentColor" x="2.55" y="2.55" width="6.9" height="6.9" rx="2" /><rect fill="currentColor" x="2.55" y="10.55" width="6.9" height="6.9" rx="2" /><rect fill="currentColor" x="10.55" y="10.55" width="6.9" height="6.9" rx="2" /><circle fill="currentColor" cx="14" cy="6" r="3.55" /></>,
  },
  driversRail: {
    viewBox: "0 0 20 20",
    node: <><path {...stroke} d="M6.7 3.1v4M13.3 3.1v4M4.6 7.1h10.8M5.5 7.1v3.4a4.5 4.5 0 0 0 9 0V7.1M10 15v2.1" /></>,
  },
  driversRailFilled: {
    viewBox: "0 0 20 20",
    node: <><path {...stroke} d="M6.7 3.1v4M13.3 3.1v4M10 15v2.1" /><path fill="currentColor" d="M4.6 6.4a.7.7 0 0 0 0 1.4h.2v2.7a5.2 5.2 0 0 0 10.4 0V7.8h.2a.7.7 0 0 0 0-1.4H4.6Z" /></>,
  },
} satisfies Partial<Record<GlyphName, GlyphDef>>;

registerGlyphPack("navigation", glyphs);
