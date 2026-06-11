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
      <path
        d="M12 2.75L20.25 7.5V16.5L12 21.25L3.75 16.5V7.5L12 2.75Z"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinejoin="round"
      />
      <circle cx="12" cy="12" r="2.25" fill="currentColor" />
    </svg>
  );
}
