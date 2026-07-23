import type { ButtonHTMLAttributes, ReactNode } from 'react';

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'primary' | 'secondary' | 'danger' | 'quiet';
  children: ReactNode;
}

export function Button({ variant = 'secondary', className = '', ...props }: ButtonProps) {
  return <button className={`button button--${variant} ${className}`} {...props} />;
}
