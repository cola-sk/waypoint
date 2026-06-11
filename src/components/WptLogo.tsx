import React from "react";

type WptLogoProps = {
  size?: number;
  className?: string;
};

export default function WptLogo({ size = 22, className = "" }: WptLogoProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      style={{
        display: "inline-block",
        verticalAlign: "middle"
      }}
    >
      {/* Flat Hexagon Outline */}
      <path
        d="M12 2L20 6.5V17.5L12 22L4 17.5V6.5L12 2Z"
        stroke="var(--accent-cyan)"
        strokeWidth="1.8"
        strokeLinejoin="round"
      />
      {/* Clean Monospace WPT text */}
      <text
        x="12.2"
        y="15.2"
        fill="var(--accent-cyan)"
        fontFamily="var(--font-mono)"
        fontSize="7.5"
        fontWeight="900"
        textAnchor="middle"
        letterSpacing="-0.2"
      >
        WPT
      </text>
    </svg>
  );
}
