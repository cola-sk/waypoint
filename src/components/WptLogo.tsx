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
        d="M12 5.25L17.85 8.62V15.38L12 18.75L6.15 15.38V8.62L12 5.25Z"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinejoin="round"
      />
      <circle cx="12" cy="12" r="1.65" fill="currentColor" />
    </svg>
  );
}
