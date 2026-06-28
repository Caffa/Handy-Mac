import React from "react";

interface BadgeProps {
  children: React.ReactNode;
  variant?: "primary" | "success" | "secondary";
  className?: string;
  title?: string;
}

const Badge: React.FC<BadgeProps> = ({
  children,
  variant = "primary",
  className = "",
  title,
}) => {
  const variantClasses = {
    primary: "bg-logo-primary",
    success: "bg-green-500/20 text-green-400",
    secondary: "bg-mid-gray/20 text-text/70",
  };

  return (
    <span
      className={`inline-flex items-center px-3 py-1 rounded-full text-xs font-medium ${variantClasses[variant]} ${className}`}
      title={title}
    >
      {children}
    </span>
  );
};

export default Badge;
