/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  darkMode: 'class',
  theme: {
    extend: {
      // Step 4 responsive-shell pass: named breakpoint for the one layout
      // decision that isn't well served by the default sm/md/lg/xl scale -
      // "is there still room for Sidebar + FileList + ConversionPanel side
      // by side, or does ConversionPanel need to stack below the file
      // list". Measured against the app's own fixed-width chrome (see
      // App.tsx's Main Content comment), not picked from a generic device
      // breakpoint table. A named screen (vs. sprinkling `min-[1050px]:`
      // across several files) keeps that one number in a single place.
      screens: {
        shell: '1050px',
      },
      colors: {
        // Phase 1 - Secure Desktop Foundation: navy, not black.
        // Every `dark-*` utility in the app (bg-dark-800, text-dark-400,
        // border-dark-700, ...) is driven from this one ramp, and it's
        // dual-purpose: high numbers are DARK MODE SURFACES, low numbers
        // are LIGHT MODE TEXT (e.g. `text-dark-900` for body text on a
        // light background). The spec asks for "dark navy text" in the
        // light theme too, so shifting the whole ramp toward navy serves
        // both correctly, from one place, without touching the ~15
        // components that each do their own `settings.theme === "dark"`
        // branching - see the comment above `resolveTheme` in useStore.ts.
        //
        // The five named tokens from the spec's suggested palette are
        // placed at the ramp position matching their role:
        //   text-primary   #F8FAFC -> 50   (lightest)
        //   text-secondary #AFC0D4 -> 300
        //   border         #29445F -> 700
        //   surface-hover  #173A5E -> 750
        //   surface        #102A46 -> 800
        //   sidebar        #091D33 -> 900
        //   background     #07182B -> 950  (deepest - replaces #000000)
        dark: {
          50: '#F8FAFC',
          100: '#EAF0F6',
          200: '#D3DFEA',
          300: '#AFC0D4',
          400: '#8A9BB3',
          500: '#5E7691',
          600: '#3E5872',
          700: '#29445F',
          750: '#173A5E',
          800: '#102A46',
          900: '#091D33',
          950: '#07182B',
        },
        // MEB red - the single accent color used in both themes (the spec
        // lists it once, under the shared palette, and separately confirms
        // "Light theme: MEB red as accent" - not a different color per
        // theme).
        accent: {
          50: '#FDECEC',
          100: '#FBD5D5',
          200: '#F7ABAB',
          300: '#F17E7E',
          400: '#EA4D4D',
          500: '#E30A17',
          600: '#C40712',
          700: '#9E050E',
          800: '#78040B',
          900: '#520207',
        },
        brand: {
          light: '#EA4D4D',
          DEFAULT: '#E30A17', // MEB red
          dark: '#9E050E',
        },
        success: {
          400: '#4ade80',
          500: '#22c55e',
          600: '#16a34a',
        },
        warning: {
          400: '#fbbf24',
          500: '#f59e0b',
          600: '#d97706',
        },
        error: {
          400: '#f87171',
          500: '#ef4444',
          600: '#dc2626',
        }
      },
      backgroundImage: {
        'gradient-radial': 'radial-gradient(var(--tw-gradient-stops))',
        'dark-gradient': 'linear-gradient(145deg, #07182B 0%, #102A46 100%)',
        'light-gradient': 'linear-gradient(145deg, #f4f4f5 0%, #fafafa 100%)',
        'card-gradient-dark': 'linear-gradient(165deg, rgba(16, 42, 70, 0.4) 0%, rgba(9, 29, 51, 0.6) 100%)',
        'card-gradient-light': 'linear-gradient(165deg, rgba(255, 255, 255, 0.8) 0%, rgba(250, 250, 250, 0.9) 100%)',
        'accent-gradient': 'linear-gradient(135deg, #E30A17 0%, #C40712 100%)',
        'glass-gradient': 'linear-gradient(180deg, rgba(255, 255, 255, 0.08) 0%, rgba(255, 255, 255, 0.03) 100%)',
      },
      boxShadow: {
        'glass': '0 8px 32px 0 rgba(0, 0, 0, 0.37)',
        'glass-light': '0 8px 32px 0 rgba(31, 38, 135, 0.07)',
        'glow': '0 0 20px -5px rgba(227, 10, 23, 0.5)',
        'glow-strong': '0 0 30px -5px rgba(196, 7, 18, 0.6)',
      },
      animation: {
        'pulse-slow': 'pulse 3s cubic-bezier(0.4, 0, 0.6, 1) infinite',
        'bounce-slow': 'bounce 2s infinite',
        'shimmer': 'shimmer 2.5s linear infinite',
        'float': 'float 6s ease-in-out infinite',
        'slide-up': 'slideUp 0.4s cubic-bezier(0.16, 1, 0.3, 1) forwards',
        'fade-in': 'fadeIn 0.3s ease-out forwards',
        'scale-in': 'scaleIn 0.2s cubic-bezier(0.16, 1, 0.3, 1) forwards',
      },
      keyframes: {
        shimmer: {
          '0%': { backgroundPosition: '-200% 0' },
          '100%': { backgroundPosition: '200% 0' },
        },
        float: {
          '0%, 100%': { transform: 'translateY(0)' },
          '50%': { transform: 'translateY(-10px)' },
        },
        slideUp: {
          '0%': { opacity: 0, transform: 'translateY(20px)' },
          '100%': { opacity: 1, transform: 'translateY(0)' },
        },
        fadeIn: {
          '0%': { opacity: 0 },
          '100%': { opacity: 1 },
        },
        scaleIn: {
          '0%': { opacity: 0, transform: 'scale(0.95)' },
          '100%': { opacity: 1, transform: 'scale(1)' },
        }
      },
      backdropBlur: {
        'xs': '2px',
        'md': '12px',
        'lg': '16px',
        'xl': '24px',
      }
    },
  },
  plugins: [],
}
