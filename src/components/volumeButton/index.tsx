import { FaVolumeUp, FaVolumeOff } from "react-icons/fa";

import "./volumeButton.scss";

export interface VolumeButtonProps {
    volumeEnabled: boolean;
    onClick: () => void;
}
/**
 * VolumeButton component
 * @param {boolean} volumeEnabled - Indicates if the volume is enabled or muted.
 * @param {function} onClick - Function to call when the button is clicked.
 * @returns {JSX.Element} The VolumeButton component.
 */
export const VolumeButton: React.FC<VolumeButtonProps> = ({ volumeEnabled, onClick }) => (
    <button className="user-volume" onClick={onClick}>
        {volumeEnabled ? <FaVolumeUp /> : <FaVolumeOff />}
    </button>
);