import React from "react";

// A brand name, not a translatable string.
const BRAND = "Voix";

// The Voix wordmark. The component keeps upstream's name so every place that
// renders the app logo (sidebar, onboarding) picks it up without being touched.
const HandyTextLogo = ({
  width,
  height,
  className,
}: {
  width?: number;
  height?: number;
  className?: string;
}) => {
  return (
    <svg
      width={width}
      height={height}
      className={className}
      viewBox="0 0 930 328"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      role="img"
      aria-label={BRAND}
    >
      {/* Same waveform-V mark as the app icon (src-tauri/icons/voix-source.svg) */}
      <g className="logo-primary">
        <rect x="40" y="44" width="40" height="112" rx="20" />
        <rect x="102" y="44" width="40" height="174" rx="20" />
        <rect x="164" y="44" width="40" height="240" rx="20" />
        <rect x="226" y="44" width="40" height="174" rx="20" />
        <rect x="288" y="44" width="40" height="112" rx="20" />
      </g>
      <text
        x="392"
        y="262"
        className="logo-primary"
        fontFamily="ui-rounded, system-ui, sans-serif"
        fontSize="270"
        fontWeight="800"
        letterSpacing="-6"
      >
        {BRAND}
      </text>
    </svg>
  );
};

export default HandyTextLogo;
