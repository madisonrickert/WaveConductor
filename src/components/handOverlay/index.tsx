import React from "react";
import { FaHandPaper, FaHandRock } from "react-icons/fa";
import "./handOverlay.scss";

export interface HandData {
  index: number;
  position: { x: number; y: number };
  pinched: boolean;
}

interface HandOverlayProps {
  hands: HandData[];
}

export const HandOverlay: React.FC<HandOverlayProps> = ({ hands }) => {
  return (
    <div className="hand-overlay">
      {hands.map((hand) => (
        <div
          key={hand.index}
          className="hand-cursor"
          style={{
            left: `${hand.position.x}px`,
            top: `${hand.position.y}px`,
          }}
        >
          {hand.pinched ? <FaHandRock /> : <FaHandPaper />}
        </div>
      ))}
    </div>
  );
};
