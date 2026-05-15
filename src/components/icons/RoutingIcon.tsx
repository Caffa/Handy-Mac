import React from "react";

interface RoutingIconProps {
  width?: number;
  height?: number;
  color?: string;
  className?: string;
}

const RoutingIcon: React.FC<RoutingIconProps> = ({
  width = 20,
  height = 20,
  color = "#f59e0b",
  className = "",
}) => {
  return (
    <svg
      width={width}
      height={height}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
    >
      <line x1="22" y1="2" x2="11" y2="13" />
      <polygon points="22 2 15 22 11 13 2 9 22 2" />
    </svg>
  );
};

export default RoutingIcon;
