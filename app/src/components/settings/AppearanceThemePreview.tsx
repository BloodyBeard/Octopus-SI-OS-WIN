function LightThemePreview() {
  return (
    <svg width={80} height={60} viewBox="0 0 80 60" fill="none" xmlns="http://www.w3.org/2000/svg" className="block size-full">
      <g clipPath="url(#clip0_95_16434)">
        <rect width={80} height={60} rx={8} fill="#F4F4F4" />
        <g filter="url(#filter0_dd_95_16434)">
          <rect x={8} y={8} width={64} height={44} rx={4} fill="white" />
          <rect x={7.75} y={7.75} width={64.5} height={44.5} rx={4.25} stroke="black" strokeOpacity={0.1} strokeWidth={0.5} />
        </g>
        <rect x={38} y={12} width={30} height={2} rx={1} fill="currentColor" />
        <rect opacity={0.1} x={24} y={18} width={30} height={2} rx={1} fill="black" />
        <rect opacity={0.1} x={24} y={22} width={18} height={2} rx={1} fill="black" />
        <rect opacity={0.1} x={24} y={26} width={36} height={2} rx={1} fill="black" />
        <rect x={29} y={32} width={39} height={2} rx={1} fill="currentColor" />
        <rect x={45} y={36} width={23} height={2} rx={1} fill="currentColor" />
        <rect x={23.75} y={41.75} width={44.5} height={6.5} rx={3.25} stroke="black" strokeOpacity={0.1} strokeWidth={0.5} />
        <circle cx={65} cy={45} r={2} fill="currentColor" />
        <g filter="url(#filter1_d_95_16434)">
          <path d="M8 12C8 9.79086 9.79086 8 12 8H20V52H12C9.79086 52 8 50.2091 8 48V12Z" fill="black" fillOpacity={0.03} shapeRendering="crispEdges" />
        </g>
        <g opacity={0.5}>
          <image href="./octopus-logo.png" x={11} y={11} width={6} height={6} style={{ filter: "invert(1)" }} />
        </g>
      </g>
      <rect x={0.25} y={0.25} width={79.5} height={59.5} rx={7.75} stroke="black" strokeOpacity={0.1} strokeWidth={0.5} />
      <defs>
        <filter id="filter0_dd_95_16434" x={-2.5} y={7.5} width={85} height={65} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect1_dropShadow_95_16434" />
          <feOffset dy={4} />
          <feGaussianBlur stdDeviation={2.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16434" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect2_dropShadow_95_16434" />
          <feOffset dy={10} />
          <feGaussianBlur stdDeviation={6.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="effect1_dropShadow_95_16434" result="effect2_dropShadow_95_16434" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect2_dropShadow_95_16434" result="shape" />
        </filter>
        <filter id="filter1_d_95_16434" x={8} y={8} width={12.5} height={44} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feOffset dx={0.5} />
          <feComposite in2="hardAlpha" operator="out" />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16434" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect1_dropShadow_95_16434" result="shape" />
        </filter>
        <clipPath id="clip0_95_16434">
          <rect width={80} height={60} rx={8} fill="white" />
        </clipPath>
      </defs>
    </svg>
  );
}

function DarkThemePreview() {
  return (
    <svg width={80} height={60} viewBox="0 0 80 60" fill="none" xmlns="http://www.w3.org/2000/svg" className="block size-full">
      <g clipPath="url(#clip0_95_16452)">
        <rect width={80} height={60} rx={8} fill="#262626" />
        <g filter="url(#filter0_dd_95_16452)">
          <rect x={8} y={8} width={64} height={44} rx={4} fill="#0D0D0D" />
          <rect x={7.75} y={7.75} width={64.5} height={44.5} rx={4.25} stroke="white" strokeOpacity={0.1} strokeWidth={0.5} />
        </g>
        <rect x={38} y={12} width={30} height={2} rx={1} fill="currentColor" />
        <rect opacity={0.3} x={24} y={18} width={30} height={2} rx={1} fill="white" />
        <rect opacity={0.3} x={24} y={22} width={18} height={2} rx={1} fill="white" />
        <rect opacity={0.3} x={24} y={26} width={36} height={2} rx={1} fill="white" />
        <rect x={29} y={32} width={39} height={2} rx={1} fill="currentColor" />
        <rect x={45} y={36} width={23} height={2} rx={1} fill="currentColor" />
        <rect x={23.75} y={41.75} width={44.5} height={6.5} rx={3.25} stroke="white" strokeOpacity={0.3} strokeWidth={0.5} />
        <circle cx={65} cy={45} r={2} fill="currentColor" />
        <g filter="url(#filter1_d_95_16452)">
          <path d="M8 12C8 9.79086 9.79086 8 12 8H20V52H12C9.79086 52 8 50.2091 8 48V12Z" fill="white" fillOpacity={0.2} shapeRendering="crispEdges" />
        </g>
        <g opacity={0.5}>
          <image href="./octopus-logo.png" x={11} y={11} width={6} height={6} />
        </g>
      </g>
      <rect x={0.25} y={0.25} width={79.5} height={59.5} rx={7.75} stroke="black" strokeOpacity={0.4} strokeWidth={0.5} />
      <defs>
        <filter id="filter0_dd_95_16452" x={-2.5} y={7.5} width={85} height={65} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect1_dropShadow_95_16452" />
          <feOffset dy={4} />
          <feGaussianBlur stdDeviation={2.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16452" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect2_dropShadow_95_16452" />
          <feOffset dy={10} />
          <feGaussianBlur stdDeviation={6.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="effect1_dropShadow_95_16452" result="effect2_dropShadow_95_16452" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect2_dropShadow_95_16452" result="shape" />
        </filter>
        <filter id="filter1_d_95_16452" x={8} y={8} width={12.5} height={44} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feOffset dx={0.5} />
          <feComposite in2="hardAlpha" operator="out" />
          <feColorMatrix type="matrix" values="0 0 0 0 1 0 0 0 0 1 0 0 0 0 1 0 0 0 0.2 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16452" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect1_dropShadow_95_16452" result="shape" />
        </filter>
        <clipPath id="clip0_95_16452">
          <rect width={80} height={60} rx={8} fill="white" />
        </clipPath>
      </defs>
    </svg>
  );
}

function SystemLightThemePreview() {
  return (
    <svg width={40} height={60} viewBox="0 0 40 60" fill="none" xmlns="http://www.w3.org/2000/svg" className="block size-full">
      <g clipPath="url(#clip0_95_16397)">
        <rect width={80} height={60} fill="#F4F4F4" />
        <g filter="url(#filter0_dd_95_16397)">
          <rect x={8} y={8} width={64} height={44} rx={4} fill="white" />
          <rect x={7.75} y={7.75} width={64.5} height={44.5} rx={4.25} stroke="black" strokeOpacity={0.1} strokeWidth={0.5} />
        </g>
        <rect x={38} y={12} width={30} height={2} rx={1} fill="currentColor" />
        <rect opacity={0.1} x={24} y={18} width={30} height={2} rx={1} fill="black" />
        <rect opacity={0.1} x={24} y={22} width={18} height={2} rx={1} fill="black" />
        <rect opacity={0.1} x={24} y={26} width={36} height={2} rx={1} fill="black" />
        <rect x={29} y={32} width={39} height={2} rx={1} fill="currentColor" />
        <rect x={23.75} y={41.75} width={44.5} height={6.5} rx={3.25} stroke="black" strokeOpacity={0.1} strokeWidth={0.5} />
        <g filter="url(#filter1_d_95_16397)">
          <path d="M8 12C8 9.79086 9.79086 8 12 8H20V52H12C9.79086 52 8 50.2091 8 48V12Z" fill="black" fillOpacity={0.03} shapeRendering="crispEdges" />
        </g>
        <g opacity={0.5}>
          <image href="./octopus-logo.png" x={11} y={11} width={6} height={6} style={{ filter: "invert(1)" }} />
        </g>
      </g>
      <defs>
        <filter id="filter0_dd_95_16397" x={-2.5} y={7.5} width={85} height={65} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect1_dropShadow_95_16397" />
          <feOffset dy={4} />
          <feGaussianBlur stdDeviation={2.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16397" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect2_dropShadow_95_16397" />
          <feOffset dy={10} />
          <feGaussianBlur stdDeviation={6.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="effect1_dropShadow_95_16397" result="effect2_dropShadow_95_16397" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect2_dropShadow_95_16397" result="shape" />
        </filter>
        <filter id="filter1_d_95_16397" x={8} y={8} width={12.5} height={44} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feOffset dx={0.5} />
          <feComposite in2="hardAlpha" operator="out" />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16397" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect1_dropShadow_95_16397" result="shape" />
        </filter>
        <clipPath id="clip0_95_16397">
          <rect width={80} height={60} fill="white" />
        </clipPath>
      </defs>
    </svg>
  );
}

function SystemDarkThemePreview() {
  return (
    <svg width={40} height={60} viewBox="0 0 40 60" fill="none" xmlns="http://www.w3.org/2000/svg" className="block size-full">
      <g clipPath="url(#clip0_95_16416)">
        <rect width={80} height={60} fill="#262626" />
        <g filter="url(#filter0_dd_95_16416)">
          <rect x={8} y={8} width={64} height={44} rx={4} fill="#0D0D0D" />
          <rect x={7.75} y={7.75} width={64.5} height={44.5} rx={4.25} stroke="white" strokeOpacity={0.1} strokeWidth={0.5} />
        </g>
        <rect x={38} y={12} width={30} height={2} rx={1} fill="currentColor" />
        <rect opacity={0.3} x={24} y={18} width={30} height={2} rx={1} fill="white" />
        <rect opacity={0.3} x={24} y={22} width={18} height={2} rx={1} fill="white" />
        <rect opacity={0.3} x={24} y={26} width={36} height={2} rx={1} fill="white" />
        <rect x={29} y={32} width={39} height={2} rx={1} fill="currentColor" />
        <rect x={23.75} y={41.75} width={44.5} height={6.5} rx={3.25} stroke="white" strokeOpacity={0.3} strokeWidth={0.5} />
        <g filter="url(#filter1_d_95_16416)">
          <path d="M8 12C8 9.79086 9.79086 8 12 8H20V52H12C9.79086 52 8 50.2091 8 48V12Z" fill="white" fillOpacity={0.2} shapeRendering="crispEdges" />
        </g>
        <g opacity={0.5}>
          <image href="./octopus-logo.png" x={11} y={11} width={6} height={6} />
        </g>
      </g>
      <defs>
        <filter id="filter0_dd_95_16416" x={-2.5} y={7.5} width={85} height={65} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect1_dropShadow_95_16416" />
          <feOffset dy={4} />
          <feGaussianBlur stdDeviation={2.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16416" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feMorphology radius={3} operator="erode" in="SourceAlpha" result="effect2_dropShadow_95_16416" />
          <feOffset dy={10} />
          <feGaussianBlur stdDeviation={6.5} />
          <feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.1 0" />
          <feBlend mode="normal" in2="effect1_dropShadow_95_16416" result="effect2_dropShadow_95_16416" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect2_dropShadow_95_16416" result="shape" />
        </filter>
        <filter id="filter1_d_95_16416" x={8} y={8} width={12.5} height={44} filterUnits="userSpaceOnUse" colorInterpolationFilters="sRGB">
          <feFlood floodOpacity={0} result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" result="hardAlpha" />
          <feOffset dx={0.5} />
          <feComposite in2="hardAlpha" operator="out" />
          <feColorMatrix type="matrix" values="0 0 0 0 1 0 0 0 0 1 0 0 0 0 1 0 0 0 0.2 0" />
          <feBlend mode="normal" in2="BackgroundImageFix" result="effect1_dropShadow_95_16416" />
          <feBlend mode="normal" in="SourceGraphic" in2="effect1_dropShadow_95_16416" result="shape" />
        </filter>
        <clipPath id="clip0_95_16416">
          <rect width={80} height={60} fill="white" />
        </clipPath>
      </defs>
    </svg>
  );
}

export function ThemePreviewGraphic({ mode }: { mode: "system" | "light" | "dark" }) {
  if (mode === "system") {
    return <span className="grid size-full grid-cols-2"><SystemLightThemePreview /><SystemDarkThemePreview /></span>;
  }
  return mode === "light" ? <LightThemePreview /> : <DarkThemePreview />;
}
